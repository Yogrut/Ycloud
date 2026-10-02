import { nextTick } from 'vue'

export const STORED_PASSWORD_MASK = '••••••'

export function selectStoredPassword(event: FocusEvent): void {
  const input = event.target as HTMLInputElement
  if (input.value === STORED_PASSWORD_MASK) nextTick(() => input.select())
}
