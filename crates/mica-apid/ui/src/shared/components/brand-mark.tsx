import { cn } from '@/shared/lib/utils'

/// The product mark: the brand icon, at tile size.
///
/// An `<img>` against the same `favicon.svg` the document links, rather than a
/// copy of its paths inlined here. One asset, carried in the bundle because a
/// device may have no route off it, and the brand's own colours stay in the
/// file -- a component may not name a colour (`verify-ui-policy.sh`), and the
/// mark is two of them and a plate.
///
/// Decorative: every place it appears, the product name is beside it, so an
/// `alt` would be the same words read twice.
export function BrandMark({ className }: { className?: string }) {
  return <img src={`${import.meta.env.BASE_URL}favicon.svg`} alt="" className={cn('size-7 flex-none rounded-md', className)} />
}
