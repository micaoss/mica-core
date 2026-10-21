import { useEffect, useSyncExternalStore, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { queryClient } from '@/app/query-client'
import { BrandMark } from '@/shared/components/brand-mark'

export type WebMode = 'network' | 'local' | 'off'

/// What the console left behind when it took the API off the network.
interface Gone { mode: Exclude<WebMode, 'network'>; host: string }

let gone: Gone | undefined
const listeners = new Set<() => void>()

function subscribe(listener: () => void) {
  listeners.add(listener)
  return () => { listeners.delete(listener) }
}

/// The API has left the network: the whole console gives way to one screen
/// that says so and how to bring it back.
export function leaveConsole(mode: Gone['mode'], host: string) {
  gone = { mode, host }
  for (const listener of listeners) listener()
}

/// For tests: the console is back.
export function resetApiGone() {
  gone = undefined
  for (const listener of listeners) listener()
}

/// The console, or, once the API has left the network, the screen in its place.
export function ApiGoneGate({ children }: { children: ReactNode }) {
  const current = useSyncExternalStore(subscribe, () => gone)
  return current ? <ApiGoneScreen gone={current} /> : children
}

function ApiGoneScreen({ gone }: { gone: Gone }) {
  const { t } = useTranslation()
  // Every page has unmounted with the router: nothing observes a query any
  // more, so dropping them leaves no request behind.
  useEffect(() => {
    void queryClient.cancelQueries()
    queryClient.clear()
  }, [])
  return (
    <main className="grid min-h-svh place-items-center bg-background p-6">
      <div className="grid max-w-lg gap-4">
        <BrandMark className="size-8" />
        <h1 className="text-xl font-semibold">{t(`access.web.gone.title.${gone.mode}`)}</h1>
        <p className="text-muted-foreground">{t('access.web.gone.body')}</p>
        <pre className="rounded-md border bg-muted p-3 font-mono text-sm">{`ssh root@${gone.host}\nmicad web network`}</pre>
      </div>
    </main>
  )
}
