import { defineAsyncComponent, type AsyncComponentLoader, type Component } from 'vue'
import PageLoadState from './components/PageLoadState.vue'

// This deadline is for page code, not API requests or file transfers.
const PAGE_CODE_TIMEOUT_MS = 30_000

/// How long a page load may run before anything is painted. Shared with the
/// admin placeholder so both decide on the same deadline.
export const PAGE_LOADING_DELAY_MS = 200

export function lazyPage<T extends Component>(loader: AsyncComponentLoader<T>): T {
  return defineAsyncComponent<T>({
    loader,
    loadingComponent: PageLoadState,
    errorComponent: PageLoadState,
    // A short deadline keeps fast loads silent; slow ones still show progress.
    delay: PAGE_LOADING_DELAY_MS,
    timeout: PAGE_CODE_TIMEOUT_MS,
    // Do not automatically reload the document or retry business operations.
    onError(_error, _retry, fail) { fail() },
  })
}
