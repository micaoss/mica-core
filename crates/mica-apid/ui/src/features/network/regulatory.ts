/// The regulatory domains an access point can be set to, Europe first, then
/// the rest by code. The 2.4 GHz channels a domain allows: 1–11 in the
/// Americas' FCC-style domains, 1–13 elsewhere.

export const EUROPE = [
  'AT', 'BE', 'BG', 'CH', 'CY', 'CZ', 'DE', 'DK', 'EE', 'ES', 'FI', 'FR', 'GB', 'GR', 'HR', 'HU', 'IE', 'IS', 'IT', 'LI',
  'LT', 'LU', 'LV', 'MC', 'ME', 'MK', 'MT', 'NL', 'NO', 'PL', 'PT', 'RO', 'RS', 'SE', 'SI', 'SK', 'AL', 'BA', 'MD', 'UA', 'TR',
]

export const ELSEWHERE = [
  'AE', 'AR', 'AU', 'BR', 'CA', 'CL', 'CN', 'CO', 'EG', 'HK', 'ID', 'IL', 'IN', 'JP', 'KR', 'MA', 'MX', 'MY', 'NZ', 'PE',
  'PH', 'QA', 'SA', 'SG', 'TH', 'TN', 'TW', 'US', 'VN', 'ZA',
]

const ELEVEN = ['US', 'CA', 'MX', 'TW', 'CO', 'PE', 'AR', 'CL']

export function channelsFor(country: string): number[] {
  const last = ELEVEN.includes(country.toUpperCase()) ? 11 : 13
  return Array.from({ length: last }, (_, at) => at + 1)
}

/// `code`'s name in `locale`, or the code itself where the runtime has none.
export function countryName(code: string, locale: string): string {
  try {
    return new Intl.DisplayNames([locale], { type: 'region' }).of(code.toUpperCase()) ?? code
  } catch {
    return code
  }
}
