import type { TrafficQuota } from '../../shared/api/admin'

export function emptyTrafficQuota(): TrafficQuota {
  return { enabled: false, upload: 0, download: 0 }
}

export function sameTrafficQuota(current: TrafficQuota, previous: TrafficQuota | undefined): boolean {
  return previous !== undefined && current.enabled === previous.enabled
    && current.upload === previous.upload && current.download === previous.download
}

export function validTrafficQuota(quota: TrafficQuota): boolean {
  return [quota.upload, quota.download].every(value => Number.isSafeInteger(value) && value >= 0)
}
