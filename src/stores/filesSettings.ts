import { defineStore } from 'pinia'
import { ref } from 'vue'
import { homeDir, documentDir, join } from '@tauri-apps/api/path'

export const useFilesSettingsStore = defineStore('filesSettings', () => {
  // User override for the screenshot folder. Empty/null falls back to platform default.
  const screenshotFolder = ref<string | null>(null)
  // Last folder the general file picker opened in. Sticky across sessions.
  const lastDocsFolder = ref<string | null>(null)

  function setScreenshotFolder(path: string | null) {
    screenshotFolder.value = path && path.trim() ? path : null
  }

  function setLastDocsFolder(path: string | null) {
    lastDocsFolder.value = path && path.trim() ? path : null
  }

  // Windows default: %USERPROFILE%\Pictures\Screenshots (Windows 11 screenshot key target)
  // macOS default: ~/Desktop (default for Cmd+Shift+3/4)
  async function getScreenshotDefaultDir(): Promise<string> {
    const isMac = navigator.platform.startsWith('Mac')
    const home = await homeDir()
    return isMac ? await join(home, 'Desktop') : await join(home, 'Pictures', 'Screenshots')
  }

  async function getDocsDefaultDir(): Promise<string> {
    try {
      return await documentDir()
    } catch {
      return await homeDir()
    }
  }

  async function resolveScreenshotDir(): Promise<string> {
    return screenshotFolder.value || await getScreenshotDefaultDir()
  }

  async function resolveDocsDir(): Promise<string> {
    return lastDocsFolder.value || await getDocsDefaultDir()
  }

  return {
    screenshotFolder,
    lastDocsFolder,
    setScreenshotFolder,
    setLastDocsFolder,
    getScreenshotDefaultDir,
    getDocsDefaultDir,
    resolveScreenshotDir,
    resolveDocsDir,
  }
})
