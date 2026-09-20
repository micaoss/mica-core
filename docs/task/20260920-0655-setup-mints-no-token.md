# 20260920-0655-setup-mints-no-token Setup stops minting an API token

- **status**: completed
- **priority**: P2
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-20 06:55

## Description

`POST /api/v1/setup` minted an API token on every call and returned its secret,
and the console showed it on a "Save your API token" step the operator had to
click through before entering. The route's own comment explained why:

> Unlike the browser wizard, this route always mints one: a caller driving
> setup over the API wants API access.

**That wizard does not exist.** `maud` is still a workspace dependency and no
apid or micad source file uses it; the React console is the only setup client
there is, and it posts to this route. So every operator who claimed a device
through the console was handed a long-lived bearer credential they never asked
for and could not decline -- one that is not rate-limited, unlike the password
path, and that most of them pasted nowhere and forgot.

The user decided the mint goes, rather than becoming a request flag: a device
that wants a token asks for one at `POST /api/v1/tokens`, authenticated by the
credential setup just created. There is one way to get a token now, and it is
the one with a listing and a revoke beside it.

Acceptance: `POST /api/v1/setup` answers 201 with the session's CSRF token and
no secret, the stored `access` subtree gains no `apiTokens` key, the console
enters on the session the device answered with, and the committed OpenAPI
document matches the binary.

## ActiveForm

Taking the token mint out of first-run setup

## Dependencies

- **blocked by**: (none)
- **blocks**: (none)

## Notes

Breaking API change, per the no-compatibility rule: `SetupToken` becomes
`SetupResult` and loses `token`. Also corrected on the way through: the route
documented "Answers 200" while returning 201, and `docs/design/apid.md` still
described the mint.

Verified in the rust image: `cargo test -p mica-apid --locked` 347 + 2 + 2
passing, `cargo clippy -p mica-apid --all-targets -- -D warnings` clean,
`crates/mica-apid/openapi.json` regenerated from the binary, and
`bash crates/mica-apid/ui/run.sh` green.

- complete: mint removed; apid suite and UI checks green
