import { describe, expect, it } from 'vitest'
import type { UploadTask } from './uploadQueue'
import { groupUploadTasksByStorage, groupUploadTasksByTicket, uploadTicketKey } from './uploadTaskGroups'

const file = new File([], 'file.txt')

function task(id: number, storageId: string, ticket?: string): UploadTask {
  return { id, storageId, ticket, file, basePath: '', relativePath: `${id}.txt`,
    targetPath: `${id}.txt`, status: 'queued', loaded: 0, error: '' }
}

describe('upload task grouping', () => {
  it('returns empty groups for an empty queue', () => {
    expect(groupUploadTasksByStorage([]).size).toBe(0)
    expect(groupUploadTasksByTicket([]).size).toBe(0)
  })

  it('preserves first-seen storage order and task order within each storage', () => {
    const tasks = [task(1, 'second'), task(2, 'first'), task(3, 'second')]
    const groups = groupUploadTasksByStorage(tasks)
    expect([...groups.keys()]).toEqual(['second', 'first'])
    expect(groups.get('second')).toEqual([tasks[0], tasks[2]])
    expect(tasks.map(item => item.id)).toEqual([1, 2, 3])
  })

  it('isolates identical tickets across storages and different tickets within a storage', () => {
    const tasks = [task(1, 'first', 'shared'), task(2, 'second', 'shared'),
      task(3, 'first', 'other'), task(4, 'first', 'shared')]
    const groups = groupUploadTasksByTicket(tasks)
    expect([...groups.values()].map(group => [group.storageId, group.ticket, group.tasks.map(item => item.id)]))
      .toEqual([['first', 'shared', [1, 4]], ['second', 'shared', [2]], ['first', 'other', [3]]])
  })

  it('skips tasks without a ticket but leaves status filtering to the caller', () => {
    const failed = { ...task(3, 'first', 'ticket'), status: 'failed' as const }
    const groups = groupUploadTasksByTicket([task(1, 'first'), task(2, 'first', ''), failed])
    expect([...groups.values()]).toEqual([{ storageId: 'first', ticket: 'ticket', tasks: [failed] }])
  })

  it('snapshots membership while retaining live task references', () => {
    const original = task(1, 'first', 'ticket')
    const source = [original]
    const group = [...groupUploadTasksByTicket(source).values()][0]!
    source.push(task(2, 'first', 'ticket'))
    original.status = 'cancelled'
    expect(group.tasks).toHaveLength(1)
    expect(group.tasks[0]).toBe(original)
    expect(group.tasks[0]?.status).toBe('cancelled')
  })

  it('uses a delimited identity rather than concatenating storage and ticket', () => {
    expect(uploadTicketKey('ab', 'c')).not.toBe(uploadTicketKey('a', 'bc'))
    expect(uploadTicketKey('first', 'ticket')).toBe('first\u0000ticket')
  })

  it('retains every task at the queue limit without copying or mutating tasks', () => {
    const tasks = Array.from({ length: 20_000 }, (_, index) => task(index, 'first', 'ticket'))
    const storage = groupUploadTasksByStorage(tasks).get('first')!
    const ticket = [...groupUploadTasksByTicket(tasks).values()][0]!.tasks
    expect(storage).toHaveLength(tasks.length)
    expect(ticket).toHaveLength(tasks.length)
    expect(storage.every((item, index) => item === tasks[index])).toBe(true)
    expect(ticket.every((item, index) => item === tasks[index])).toBe(true)
  })
})
