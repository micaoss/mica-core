# 20260920-1035-console-brand-mark The console wears the brand mark

- **status**: completed
- **priority**: P3
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-20 10:35

## Description

The console's mark was a lowercase `m` in a tinted tile drawn from
`--logo`/`--logo-foreground` -- a stand-in from before the brand assets were in
the image. The real mark ships beside it already: `favicon.svg`, the icon
micaos.dev serves, carried in the bundle by 20260920-0811.

Three places wear it: the header, the mobile navigation sheet and the sign-in
plate.

The mark is an `<img>` against `${import.meta.env.BASE_URL}favicon.svg` rather
than a copy of its paths inlined in a component. One asset instead of two
spellings of it, and the brand's colours stay in the file: a component may not
name a colour (`verify-ui-policy.sh` check 5), and the mark is two of them and
a plate.

Acceptance: no `logo-mark` tile and no `--logo` tokens remain, the three places
render the brand icon, `crates/mica-apid/ui/run.sh` stays green.

## ActiveForm

Putting the brand mark on the console

## Dependencies

- **blocked by**: (none)
- **blocks**: (none)

- complete: BrandMark in the header, the sheet and the sign-in plate; logo-mark tile and --logo tokens removed
