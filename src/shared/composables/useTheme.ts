import { onMounted, ref } from 'vue'

type Theme = 'light' | 'dark'

const THEME_STORAGE_KEY = 'renderloft-theme'
const storedTheme = localStorage.getItem(THEME_STORAGE_KEY)
const theme = ref<Theme>(storedTheme === 'light' || storedTheme === 'dark' ? storedTheme : 'dark')

export function useTheme() {
  const applyTheme = () => {
    document.documentElement.classList.toggle('dark', theme.value === 'dark')
  }

  const setTheme = (newTheme: Theme) => {
    theme.value = newTheme
    localStorage.setItem(THEME_STORAGE_KEY, newTheme)
    applyTheme()
  }

  const toggleTheme = () => {
    setTheme(theme.value === 'light' ? 'dark' : 'light')
  }

  onMounted(applyTheme)

  return {
    theme,
    setTheme,
    toggleTheme
  }
}
