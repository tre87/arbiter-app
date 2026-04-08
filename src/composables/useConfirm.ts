import { ref } from 'vue'

interface ConfirmOptions {
  title: string
  message?: string
  confirmText?: string
  cancelText?: string
  danger?: boolean
}

interface PendingConfirm extends ConfirmOptions {
  resolve: (value: boolean) => void
}

const pending = ref<PendingConfirm | null>(null)

export function useConfirm() {
  function confirm(options: ConfirmOptions): Promise<boolean> {
    return new Promise((resolve) => {
      pending.value = { ...options, resolve }
    })
  }

  function resolve(value: boolean) {
    if (!pending.value) return
    pending.value.resolve(value)
    pending.value = null
  }

  return { pending, confirm, resolve }
}
