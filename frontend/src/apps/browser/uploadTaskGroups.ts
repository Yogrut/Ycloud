import type { UploadTask } from './uploadQueue'

interface UploadTicketGroup {
  storageId: string
  ticket: string
  tasks: UploadTask[]
}

export function uploadTicketKey(storageId: string, ticket: string): string {
  return `${storageId}\u0000${ticket}`
}

// These are snapshots of membership, not of task state: pause/cancel can still
// update the original tasks while an API request is in flight.
export function groupUploadTasksByStorage(tasks: readonly UploadTask[]): Map<string, UploadTask[]> {
  const groups = new Map<string, UploadTask[]>()
  for (const task of tasks) {
    const group = groups.get(task.storageId)
    if (group) group.push(task)
    else groups.set(task.storageId, [task])
  }
  return groups
}

export function groupUploadTasksByTicket(tasks: readonly UploadTask[]): Map<string, UploadTicketGroup> {
  const groups = new Map<string, UploadTicketGroup>()
  for (const task of tasks) {
    if (!task.ticket) continue
    const key = uploadTicketKey(task.storageId, task.ticket)
    const group = groups.get(key)
    if (group) group.tasks.push(task)
    else groups.set(key, { storageId: task.storageId, ticket: task.ticket, tasks: [task] })
  }
  return groups
}
