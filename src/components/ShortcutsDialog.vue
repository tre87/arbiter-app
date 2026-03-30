<script setup lang="ts">
defineEmits<{ close: [] }>()

const shortcuts = [
  { action: 'New workspace', keys: ['Ctrl', 'T'] },
  { action: 'Next workspace', keys: ['Ctrl', 'Tab'] },
  { action: 'Previous workspace', keys: ['Ctrl', 'Shift', 'Tab'] },
  { action: 'Switch to workspace 1-9', keys: ['Ctrl', '1-9'] },
  { action: 'Close pane', keys: ['Ctrl', 'Shift', 'W'] },
  { action: 'Split right', keys: ['Ctrl', 'Shift', 'R'] },
  { action: 'Split down', keys: ['Ctrl', 'Shift', 'D'] },
  { action: 'Navigate panes', keys: ['Ctrl', 'Shift', 'Arrow'] },
  { action: 'Resize panes', keys: ['Alt', 'Shift', 'Arrow'] },
]
</script>

<template>
  <div class="dialog-overlay" @mousedown.self="$emit('close')">
    <div class="dialog">
      <h3 class="dialog-title">Keyboard Shortcuts</h3>
      <table class="shortcuts-table">
        <thead>
          <tr>
            <th>Action</th>
            <th>Keybind</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="s in shortcuts" :key="s.action">
            <td class="action-cell">{{ s.action }}</td>
            <td class="keys-cell">
              <kbd>{{ s.keys.join('+') }}</kbd>
            </td>
          </tr>
        </tbody>
      </table>
      <div class="dialog-actions">
        <button class="btn btn-primary" @click="$emit('close')">Close</button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.dialog-overlay {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.6);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 9999;
}

.dialog {
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-card-border);
  border-radius: 8px;
  padding: 20px 24px;
  min-width: 340px;
  box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
}

.dialog-title {
  margin: 0 0 16px;
  font-size: 14px;
  font-weight: 600;
  color: var(--color-text-primary);
}

.shortcuts-table {
  width: 100%;
  border-collapse: collapse;
}

.shortcuts-table th {
  text-align: left;
  font-size: 11px;
  font-weight: 500;
  color: var(--color-text-muted);
  padding: 6px 0;
  border-bottom: 1px solid var(--color-card-border);
}

.shortcuts-table td {
  padding: 10px 0;
  border-bottom: 1px solid var(--color-card-border);
}

.shortcuts-table tr:last-child td {
  border-bottom: none;
}

.action-cell {
  font-size: 13px;
  color: var(--color-text-secondary);
}

.keys-cell {
  text-align: right;
}

kbd {
  display: inline-block;
  font-family: inherit;
  font-size: 11px;
  color: #f87171;
  background: rgba(248, 113, 113, 0.1);
  border: 1px solid rgba(248, 113, 113, 0.25);
  border-radius: 4px;
  padding: 2px 8px;
  letter-spacing: 0.3px;
}

.dialog-actions {
  display: flex;
  justify-content: flex-end;
  margin-top: 16px;
}

.btn-primary {
  padding: 6px 16px;
  border-radius: 4px;
  font-size: 12px;
  font-weight: 500;
  cursor: pointer;
  background: var(--color-accent);
  color: #fff;
  border: 1px solid var(--color-accent);
  transition: background 0.15s, border-color 0.15s;
}

.btn-primary:hover {
  background: var(--azure-deep);
  border-color: var(--azure-deep);
}
</style>
