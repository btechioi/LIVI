import net from 'node:net'
import type { Config } from '@shared/types/Config'

/** What the Wi-Fi interface list carries for the dongle's own access point. */
export const DONGLE_AP = 'livi-link'

const HOST = 'livi-link.local'
const PORT = 5001
/** Applying waits for the radio, and a 5 GHz start spends the first seconds scanning. */
const APPLY_MS = 30_000
const PROBE_MS = 1500

/**
 * Runs commands on one connection, in order, and gives up on the first one the dongle refuses.
 * Every answer ends in `ok` or `error <reason>`, so the next command goes out on the `ok`.
 */
function talk(commands: string[], timeoutMs = APPLY_MS): Promise<string[]> {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection({ host: HOST, port: PORT })
    const answers: string[] = []
    let buffer = ''
    let at = 0
    let done = false

    const finish = (err?: Error): void => {
      if (done) return
      done = true
      clearTimeout(timer)
      socket.destroy()
      if (err) reject(err)
      else resolve(answers)
    }
    // One timer for the whole exchange, since a name that does not resolve never reaches connect.
    const timer = setTimeout(() => finish(new Error('the dongle did not answer')), timeoutMs)

    socket.on('connect', () => socket.write(`${commands[at]}\n`))
    socket.on('data', (chunk) => {
      buffer += chunk.toString()
      for (let end = buffer.indexOf('\n'); end >= 0; end = buffer.indexOf('\n')) {
        const line = buffer.slice(0, end).trimEnd()
        buffer = buffer.slice(end + 1)
        if (line.startsWith('error ')) {
          finish(new Error(`${commands[at]}: ${line.slice(6)}`))
          return
        }
        if (line !== 'ok') {
          answers.push(line)
          continue
        }
        at += 1
        if (at >= commands.length) {
          finish()
          return
        }
        socket.write(`${commands[at]}\n`)
      }
    })
    socket.on('error', (err) => finish(err))
    socket.on('close', () => finish(new Error('the dongle closed the link')))
  })
}

/** What the dongle is told for this configuration. */
export function commandsFor(config: Config): string[] {
  if (config.wifiInterface !== DONGLE_AP) {
    // Its radios would only sit next to the ones actually in use.
    return ['off', 'bt off']
  }
  return [
    `set ssid ${config.carName || 'LIVI'}`,
    `set country ${config.country || 'DE'}`,
    `set channel ${config.wifiChannel || 36}`,
    `set passphrase ${config.wifiPassword || '12345678'}`,
    'apply',
    // Keeps the whole state across a reboot. Only a boot that reaches neither the USB link nor
    // the AP puts the default name back.
    'save',
    'bt on'
  ]
}

/** Whether a LIVI Link is on the network and ready to be configured. */
export async function dongleApPresent(): Promise<boolean> {
  try {
    await talk(['status'], PROBE_MS)
    return true
  } catch {
    return false
  }
}

/** Hands the dongle its settings when it is the chosen AP, and silences it when it is not. */
export async function reconcileDongleAp(config: Config): Promise<void> {
  try {
    await talk(commandsFor(config))
  } catch (err) {
    // No dongle, or one that refused the change. Either way nothing local depends on it.
    console.warn('[dongleAp]', String(err))
  }
}
