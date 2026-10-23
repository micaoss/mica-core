import { Fragment, type ReactNode } from 'react'
import { Item, ItemContent, ItemGroup, ItemTitle } from '@/shared/components/ui/item'
import { cn } from '@/shared/lib/utils'

export interface Fact {
  id: string
  label: ReactNode
  value: ReactNode
  mono?: boolean
}

/// Label-and-value readings of what the device reported. Four spellings of this
/// existed — `.details`, `.fact-grid`, `.compact-details`, `.review-list` — and
/// they disagreed about alignment, wrapping and whether a long identifier could
/// break.
/// Falsy entries are dropped, so a caller can write `value && { ... }` against
/// an optional field without first converting it to a boolean.
///
/// `rows` puts each value at the far end of its label's row; `pairs` keeps
/// the value beside a 160 px label column; `grid` stacks each label over its
/// value in up to three columns.
export function FactList({ facts, className, layout = 'rows' }: { facts: (Fact | undefined | false | '' | null)[]; className?: string; layout?: 'rows' | 'pairs' | 'grid' }) {
  const present = facts.filter((fact): fact is Fact => Boolean(fact))
  if (present.length === 0) return null
  if (layout === 'pairs') {
    return (
      <dl className={cn('grid grid-cols-[160px_minmax(0,1fr)] gap-x-4 gap-y-2.5 text-sm', className)}>
        {present.map((fact) => (
          <Fragment key={fact.id}>
            <dt className="text-muted-foreground">{fact.label}</dt>
            <dd className={cn('min-w-0 break-words', fact.mono && 'font-mono text-[0.8125rem]')}>{fact.value}</dd>
          </Fragment>
        ))}
      </dl>
    )
  }
  if (layout === 'grid') {
    return (
      <dl className={cn('grid gap-x-6 gap-y-4 sm:grid-cols-2 lg:grid-cols-3', className)}>
        {present.map((fact) => (
          <div key={fact.id} className="grid min-w-0 content-start gap-1">
            <dt className="text-[0.8125rem] text-muted-foreground">{fact.label}</dt>
            <dd className={cn('text-sm break-words', fact.mono && 'font-mono text-[0.8125rem]')}>{fact.value}</dd>
          </div>
        ))}
      </dl>
    )
  }
  return (
    <ItemGroup className={cn('divide-y', className)}>
      {present.map((fact) => (
        <Item key={fact.id} size="xs" className="justify-between gap-6 rounded-none px-0">
          <ItemContent className="flex-none basis-auto">
            <ItemTitle className="font-normal text-muted-foreground">{fact.label}</ItemTitle>
          </ItemContent>
          <div className={cn('min-w-0 text-right text-sm break-words', fact.mono && 'font-mono text-[0.8125rem]')}>
            {fact.value}
          </div>
        </Item>
      ))}
    </ItemGroup>
  )
}
