/**
 * Local type shim for `react-dom/client`. The module is a platform external
 * in every DSH client bundle (PLATFORM_MODULES), but react-dom is not in this
 * package's devDependencies — only the two members the body-portal fallback
 * uses are declared. Remove when react-dom joins the devDependencies.
 */
declare module 'react-dom/client' {
  import type { ReactNode } from 'react'
  export interface Root {
    render(children: ReactNode): void
    unmount(): void
  }
  export function createRoot(container: Element | DocumentFragment): Root
}
