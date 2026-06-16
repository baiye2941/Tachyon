import { useI18n } from '../../i18n'
import Button from './Button'

export default function LanguageSwitcher() {
  const i18n = useI18n()

  const nextLocale = () => (i18n.language === 'zh-CN' ? 'en-US' : 'zh-CN')
  const label = () => (i18n.language === 'zh-CN' ? 'EN' : '中')

  return (
    <Button
      variant="ghost"
      size="sm"
      onClick={() => i18n.setLanguage(nextLocale())}
      title={i18n.language === 'zh-CN' ? 'Switch to English' : '切换到中文'}
      aria-label="切换语言 / Switch language"
    >
      {label()}
    </Button>
  )
}
