import { shallowRef } from 'vue'

// A newer result replaces the previous notification instead of covering it.
export const activeFeedback = shallowRef<symbol>()
