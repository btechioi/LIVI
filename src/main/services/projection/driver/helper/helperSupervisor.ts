import { type ChildProcess, execFileSync, spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { EventEmitter } from 'node:events'
import { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync, renameSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { DEBUG } from '@main/constants'
import type { Config } from '@shared/types'
import { app } from 'electron'
import { loadOrCreateIdentity } from '../cp/stack/identity'

const HELPER_BIN = 'livi-helperd'

// The helper runs as root (privileged Bluetooth/GPIO/usb). Root can exec a
// binary from a real filesystem path, but not from an AppImage FUSE mount.
// Staging every build flavour (dev, unpacked, AppImage, installed) into one
// canonical userData path keeps the sudoers grant a single stable Cmnd_Alias.
// Staged via temp + rename: the AP boot service runs this binary, and writing
// the executing inode in place raises ETXTBSY — rename swaps the path
// atomically instead.
function stageHelperBin(src: string): string {
  const dest = join(app.getPath('userData'), 'driver', HELPER_BIN)
  const digest = (p: string): string => createHash('sha256').update(readFileSync(p)).digest('hex')
  if (!existsSync(dest) || digest(dest) !== digest(src)) {
    mkdirSync(dirname(dest), { recursive: true })
    const tmp = `${dest}.new`
    copyFileSync(src, tmp)
    chmodSync(tmp, 0o755)
    renameSync(tmp, dest)
  }
  return dest
}

// A leftover helper (crashed LIVI, killed session) runs as root and keeps the
// MFi GPIO, the RFCOMM channel and the BT sockets claimed — BlueZ then rejects
// a fresh start with "UUID already registered" until it is gone. Only root can
// end it.  The patterns are end-anchored so the pkill invocation (and its sudo
// wrapper, whose own command line ends in the pattern text) never matches its
// own command line, and the anchor spares the AP unit, which runs the same
// binary with a --wifi-ap suffix.
// The kill grant is part of the 99-LIVI-bt sudoers drop-in (LIVI_BT_KILL alias).
function killStaleHelpers(): void {
  if (process.platform !== 'linux') return
  for (const pattern of ['livi-helper.py$', 'driver/livi-helperd$']) {
    try {
      // pkill exits 0 only when it actually signalled a match.
      execFileSync('sudo', ['-n', 'pkill', '-f', pattern], { stdio: 'ignore' })
      console.warn(`[helper] stopped a stale helper instance (${pattern})`)
    } catch {
      // exit 1 = nothing stale was running; other codes = grant not installed yet.
    }
  }
}

function resolveHelperBin(): string {
  const envBin = process.env.LIVI_HELPER_BIN
  if (envBin && existsSync(envBin)) return envBin

  const staged = join(app.getPath('userData'), 'driver', HELPER_BIN)
  const resBin =
    typeof process.resourcesPath === 'string'
      ? join(process.resourcesPath, 'driver', HELPER_BIN)
      : ''
  const exportBin = existsSync(resBin) ? resBin : join(__dirname, 'driver', HELPER_BIN)
  if (existsSync(exportBin)) {
    if (process.platform !== 'linux') return exportBin
    try {
      return stageHelperBin(exportBin)
    } catch (err) {
      console.warn(`[helper] staging failed: ${(err as Error).message}`)
      // A previously staged copy, even an older build, is the only path sudo
      // can run without a fresh grant — prefer it over a fallback location.
      if (existsSync(staged)) return staged
      return exportBin
    }
  }
  return join(__dirname, 'driver', HELPER_BIN)
}
function envFromConfig(cfg: Config): NodeJS.ProcessEnv {
  const wantAaWireless = cfg.wirelessAaEnabled === true
  const wantCpWireless = cfg.wirelessCpEnabled === true
  const identity = loadOrCreateIdentity()

  return {
    ...process.env,
    LIVI_AA_WIRELESS: wantAaWireless ? '1' : '',
    LIVI_CP_WIRELESS: wantCpWireless ? '1' : '',
    DEBUG: DEBUG ? '1' : '',
    LIVI_CP_PK: identity.pkHex,
    LIVI_CP_PI: identity.pairingId,
    LIVI_CP_NAME: cfg.carName || process.env.LIVI_CP_NAME || '',
    LIVI_CP_DEBUG: DEBUG ? '1' : ''
  }
}

export interface HelperSupervisorEvents {
  stdout: (line: string) => void
  stderr: (line: string) => void
  exit: (code: number | null, signal: NodeJS.Signals | null) => void
  error: (err: Error) => void
}

export interface HelperSupervisorOptions {
  restartDelayMs?: number
  maxRestarts?: number
}
export class HelperSupervisor extends EventEmitter {
  private _child: ChildProcess | null = null
  private _stopped = false
  private _restartCount = 0
  private _restartTimer: NodeJS.Timeout | null = null
  private _cfg: Config | null = null
  private readonly _restartDelayMs: number
  private readonly _maxRestarts: number

  constructor(opts: HelperSupervisorOptions = {}) {
    super()
    this._restartDelayMs = opts.restartDelayMs ?? 2000
    this._maxRestarts = opts.maxRestarts ?? -1
  }

  start(cfg: Config): void {
    this._stopped = false
    this._cfg = cfg
    this._restartCount = 0
    this._spawn()
  }

  async stop(): Promise<void> {
    this._stopped = true
    if (this._restartTimer) {
      clearTimeout(this._restartTimer)
      this._restartTimer = null
    }
    const child = this._child
    if (!child || child.exitCode !== null) return
    await new Promise<void>((resolve) => {
      const onExit = (): void => resolve()
      child.once('exit', onExit)
      child.kill('SIGTERM')
      setTimeout(() => {
        if (child.exitCode === null && !child.killed) child.kill('SIGKILL')
      }, 3000).unref?.()
    })
    this._child = null
  }

  get running(): boolean {
    return this._child !== null && this._child.exitCode === null
  }

  private _spawn(): void {
    if (!this._cfg) return
    killStaleHelpers()
    const bin = resolveHelperBin()

    if (!existsSync(bin)) {
      this.emit('error', new Error(`${HELPER_BIN} not found at ${bin}`))
      return
    }

    const env = envFromConfig(this._cfg)
    const useSudo = process.platform === 'linux'
    const cmd = useSudo ? 'sudo' : bin
    const args = useSudo ? ['-n', '-E', bin] : []

    if (DEBUG) {
      console.log(
        `[helper] spawning ${cmd} ${args.join(' ')} (aa=${env.LIVI_AA_WIRELESS || '0'}, cpWireless=${env.LIVI_CP_WIRELESS || '0'})`
      )
    }

    const child = spawn(cmd, args, {
      cwd: dirname(bin),
      env,
      stdio: ['ignore', 'pipe', 'pipe']
    })

    this._child = child
    child.stdout?.setEncoding('utf8')
    child.stderr?.setEncoding('utf8')

    let outBuf = ''
    child.stdout?.on('data', (chunk: string) => {
      outBuf += chunk
      let nl = outBuf.indexOf('\n')
      while (nl !== -1) {
        const line = outBuf.slice(0, nl).replace(/\r$/, '')
        outBuf = outBuf.slice(nl + 1)
        if (line.length > 0) this.emit('stdout', line)
        nl = outBuf.indexOf('\n')
      }
    })

    let errBuf = ''
    child.stderr?.on('data', (chunk: string) => {
      errBuf += chunk
      let nl = errBuf.indexOf('\n')
      while (nl !== -1) {
        const line = errBuf.slice(0, nl).replace(/\r$/, '')
        errBuf = errBuf.slice(nl + 1)
        if (line.length > 0) this.emit('stderr', line)
        nl = errBuf.indexOf('\n')
      }
    })

    child.on('error', (err) => {
      if (DEBUG) console.warn(`[bt] child error: ${err.message}`)
      this.emit('error', err)
    })

    child.on('exit', (code, signal) => {
      if (DEBUG) console.log(`[bt] child exited code=${code} signal=${signal}`)
      this.emit('exit', code, signal)
      this._child = null

      if (this._stopped) return

      this._restartCount += 1
      if (this._maxRestarts >= 0 && this._restartCount > this._maxRestarts) {
        this.emit('error', new Error(`${HELPER_BIN} exceeded max restarts (${this._maxRestarts})`))
        return
      }

      this._restartTimer = setTimeout(() => {
        this._restartTimer = null
        this._spawn()
      }, this._restartDelayMs)
      this._restartTimer.unref?.()
    })
  }
}
