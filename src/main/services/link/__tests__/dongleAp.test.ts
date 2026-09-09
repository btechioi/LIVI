import { EventEmitter } from 'node:events'
import type { Config } from '@shared/types/Config'

class MockSocket extends EventEmitter {
  sent: string[] = []
  destroyed = false
  write(text: string): boolean {
    this.sent.push(text)
    return true
  }
  destroy(): void {
    this.destroyed = true
  }
  /** The dongle answering, one chunk as it would arrive. */
  say(text: string): void {
    this.emit('data', Buffer.from(text))
  }
}

const { sockets, createConnection } = vi.hoisted(() => {
  const list: MockSocket[] = []
  return {
    sockets: list,
    createConnection: vi.fn(() => {
      const s = new MockSocket()
      list.push(s)
      // The caller writes on connect, so the event has to land after it subscribed.
      queueMicrotask(() => s.emit('connect'))
      return s
    })
  }
})

vi.mock('node:net', () => ({ default: { createConnection }, createConnection }))

import { commandsFor, DONGLE_AP, dongleApPresent, reconcileDongleAp } from '../dongleAp'

const config = {
  wifiInterface: 'wlan0',
  carName: 'Volvo',
  country: 'DE',
  wifiChannel: 44,
  wifiPassword: 'geheim12'
} as Config

beforeEach(() => {
  sockets.length = 0
  createConnection.mockClear()
})

/** Waits for the command to go out, then answers it. */
async function answer(socket: MockSocket, count: number, text = 'ok\n'): Promise<void> {
  for (let i = 0; i < 200 && socket.sent.length < count; i++) await Promise.resolve()
  socket.say(text)
}

describe('what the dongle is told', () => {
  it('silences it while something else is the access point', () => {
    expect(commandsFor(config)).toEqual(['off', 'bt off'])
  })

  it('hands over the settings once it is the access point', () => {
    expect(commandsFor({ ...config, wifiInterface: DONGLE_AP })).toEqual([
      'set ssid Volvo',
      'set country DE',
      'set channel 44',
      'set passphrase geheim12',
      'apply',
      'save',
      'bt on'
    ])
  })

  it('stands in for a setting that was left empty', () => {
    const bare = { ...config, wifiInterface: DONGLE_AP, carName: '', wifiPassword: '' } as Config
    expect(commandsFor(bare)).toContain('set ssid LIVI')
    expect(commandsFor(bare)).toContain('set passphrase 12345678')
  })
})

describe('talking to the dongle', () => {
  it('sends the next command only after the one before was taken', async () => {
    const done = reconcileDongleAp(config)
    const socket = sockets[0]
    await answer(socket, 1)
    await answer(socket, 2)
    await done
    expect(socket.sent).toEqual(['off\n', 'bt off\n'])
    expect(socket.destroyed).toBe(true)
  })

  it('gives up on a refusal instead of carrying on', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const done = reconcileDongleAp({ ...config, wifiInterface: DONGLE_AP })
    const socket = sockets[0]
    await answer(socket, 1, 'error channel is out of range\n')
    await done
    expect(socket.sent).toEqual(['set ssid Volvo\n'])
    expect(warn.mock.calls[0]?.[1]).toContain('channel is out of range')
    warn.mockRestore()
  })

  it('reads the lines an answer carries before its ok', async () => {
    const probe = dongleApPresent()
    const socket = sockets[0]
    await answer(socket, 1, 'state on\nbt off\nok\n')
    expect(await probe).toBe(true)
  })

  it('reports no dongle when the link fails', async () => {
    const probe = dongleApPresent()
    for (let i = 0; i < 50 && sockets.length === 0; i++) await Promise.resolve()
    sockets[0].emit('error', new Error('ENOTFOUND'))
    expect(await probe).toBe(false)
  })
})
