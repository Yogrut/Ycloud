import { defineAsyncComponent, type AsyncComponentLoader, type Component } from 'vue'
import PageLoadState from './components/PageLoadState.vue'

// This deadline is for page code, not API requests or file transfers.
const PAGE_CODE_TIMEOUT_MS = 30_000

export function lazyPage<T extends Component>(loader: AsyncComponentLoader<T>): T {
  return defineAsyncComponent<T>({
    loader,
    loadingComponent: PageLoadState,
    errorComponent: PageLoadState,
    delay: 0,
    timeout: PAGE_CODE_TIMEOUT_MS,
    // Do not automatically reload the document or retry business operations.
    onError(_error, _retry, fail) { fail() },
  })
}
