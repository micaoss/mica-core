/// The regulatory domains an access point can be set to, Europe first, then
/// the rest by code.

export const EUROPE = [
  'AT', 'BE', 'BG', 'CH', 'CY', 'CZ', 'DE', 'DK', 'EE', 'ES', 'FI', 'FR', 'GB', 'GR', 'HR', 'HU', 'IE', 'IS', 'IT', 'LI',
  'LT', 'LU', 'LV', 'MC', 'ME', 'MK', 'MT', 'NL', 'NO', 'PL', 'PT', 'RO', 'RS', 'SE', 'SI', 'SK', 'AL', 'BA', 'MD', 'UA', 'TR',
]

export const ELSEWHERE = [
  'AE', 'AR', 'AU', 'BR', 'CA', 'CL', 'CN', 'CO', 'EG', 'HK', 'ID', 'IL', 'IN', 'JP', 'KR', 'MA', 'MX', 'MY', 'NZ', 'PE',
  'PH', 'QA', 'SA', 'SG', 'TH', 'TN', 'TW', 'US', 'VN', 'ZA',
]

/// `code`'s name in `locale`, or the code itself where the runtime has none.
export function countryName(code: string, locale: string): string {
  try {
    return new Intl.DisplayNames([locale], { type: 'region' }).of(code.toUpperCase()) ?? code
  } catch {
    return code
  }
}
