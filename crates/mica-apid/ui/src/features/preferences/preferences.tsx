import { useMemo } from 'react'
import { Globe, Moon, Sun, SunMoon } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { currentLocaleChoice, setLocaleChoice } from '@/i18n/i18n'
import { AUTO_LOCALE, autoLanguageNative, languageChoices } from '@/i18n/locale'
import { useTheme, type ThemeMode } from '@/theme/theme'
import { StatusBadge } from '@/shared/components/status-badge'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/shared/components/ui/select'

/// Both pickers are icon-triggered selects, the shape micaos.dev uses for the
/// same two controls: the icon carries the meaning, the accessible name is on
/// the trigger, and the current value is the checked item in the popup.
///
/// The language picker used to be a combobox with a text input. In a 160px
/// chrome slot that read as an empty search box rather than as the language
/// the console is in, and it was the only control in the header that did not
/// look like the others.
const TRIGGER = 'flex-none gap-1 border-transparent bg-transparent px-2 text-sm'

function browserLocales(): readonly string[] {
  if (typeof navigator === 'undefined') return []
  return navigator.languages?.length ? navigator.languages : [navigator.language]
}

interface Choice {
  id: string
  native: string
  sub: string
  planned: boolean
}

/// The language picker.
export function LanguageControl({ className }: { className?: string }) {
  const { t } = useTranslation()
  const choice = currentLocaleChoice()
  const auto = `${t('preferences.auto')} · ${autoLanguageNative(browserLocales())}`

  const choices = useMemo<Choice[]>(() => languageChoices().map((entry) => entry.id === AUTO_LOCALE
    ? { id: entry.id, native: t('preferences.auto'), sub: auto, planned: false }
    : { id: entry.id, native: entry.native, sub: entry.english, planned: entry.planned }),
  [t, auto])

  return (
    <Select
      items={choices.map((entry) => ({ value: entry.id, label: entry.native }))}
      value={choice}
      onValueChange={(value: string | null) => { if (value) void setLocaleChoice(value) }}
    >
      <SelectTrigger aria-label={t('preferences.language')} className={`${TRIGGER} ${className ?? ''}`}>
        <Globe className="size-4" strokeWidth={1.5} aria-hidden="true" />
        {/* The chosen language is the checked item in the popup; spelling it
            on the trigger as well is what made this control wide enough to
            crowd the header. */}
        <SelectValue className="sr-only" />
      </SelectTrigger>
      <SelectContent className="w-64 min-w-0">
        {choices.map((entry) => (
          <SelectItem key={entry.id} value={entry.id}>
            <span className="flex min-w-0 flex-col">
              <span className="truncate">{entry.native}</span>
              <span className="truncate text-xs text-muted-foreground">{entry.sub}</span>
            </span>
            {entry.planned ? <StatusBadge>{t('preferences.planned')}</StatusBadge> : null}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  )
}

const THEME_MODES = ['system', 'light', 'dark'] as const
const THEME_ICONS = { system: SunMoon, light: Sun, dark: Moon }

/// The appearance picker, in the same shape as the language one.
export function ThemeControl({ className }: { className?: string }) {
  const { t } = useTranslation()
  const { mode, setMode } = useTheme()
  const Icon = THEME_ICONS[mode]

  return (
    <Select
      items={THEME_MODES.map((value) => ({ value, label: t(`preferences.themes.${value}`) }))}
      value={mode}
      onValueChange={(value: string | null) => { if (value) setMode(value as ThemeMode) }}
    >
      <SelectTrigger aria-label={t('preferences.appearance')} className={`${TRIGGER} ${className ?? ''}`}>
        {/* The icon is the current mode, so the trigger says which one is on
            without spending header width on the word. */}
        <Icon className="size-4" strokeWidth={1.5} aria-hidden="true" />
        <SelectValue className="sr-only" />
      </SelectTrigger>
      <SelectContent className="w-auto min-w-0">
        {THEME_MODES.map((value) => {
          const ItemIcon = THEME_ICONS[value]
          return (
            <SelectItem key={value} value={value}>
              <ItemIcon className="size-4" strokeWidth={1.5} aria-hidden="true" />
              {t(`preferences.themes.${value}`)}
            </SelectItem>
          )
        })}
      </SelectContent>
    </Select>
  )
}

/// The pair shown on the sign-in screen, before there is a shell to hold them.
export function Preferences() {
  const { t } = useTranslation()
  return (
    <section className="flex items-center gap-1" aria-label={t('preferences.regionLabel')}>
      <LanguageControl />
      <ThemeControl />
    </section>
  )
}
