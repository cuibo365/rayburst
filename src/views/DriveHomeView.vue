<script setup lang="ts">
/**
 * Drive home (首页): all storages rendered like a folder file list
 * (文件名 / 大小 / 类型 / 修改时间). Clicking a row opens the storage's
 * root folder in the browse view.
 */
import { onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { NEmpty, NIcon, NSpin, NTable, NButton } from 'naive-ui'
import { FolderOpenOutline, CloudOutline } from '@vicons/ionicons5'
import { invoke } from '@tauri-apps/api/core'

interface CloudStorage {
  id: number
  mount_path: string
  driver: string
  addition: string
  status: string
  disabled: boolean
  remark: string
  sort_order: number
  created_at: string
  modified: string
}

const router = useRouter()
const storages = ref<CloudStorage[]>([])
const loading = ref(false)
const error = ref('')

function fmtTime(raw: string): string {
  const d = new Date(raw.includes('T') ? raw : raw.replace(' ', 'T') + 'Z')
  if (Number.isNaN(d.getTime())) return raw
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`
}

async function load() {
  loading.value = true
  error.value = ''
  try {
    storages.value = await invoke<CloudStorage[]>('cloud_storage_list')
  } catch (e) {
    error.value = ((e as Record<string, unknown>).message as string) ?? String(e)
  } finally {
    loading.value = false
  }
}

function openStorage(s: CloudStorage) {
  if (s.disabled) return
  void router.push({ name: 'cloud-drive-browse', params: { storageId: String(s.id) } })
}

onMounted(load)
</script>

<template>
  <div class="drive-home">
    <div class="drive-home-toolbar">
      <p class="drive-home-hint">已挂载 {{ storages.length }} 个存储，点击行进入</p>
      <NButton size="small" secondary @click="load">刷新</NButton>
    </div>

    <div class="drive-home-list">
      <NSpin :show="loading">
        <NEmpty
          v-if="!loading && storages.length === 0"
          description="暂无存储，请到「存储」页添加"
          class="drive-home-empty"
        />
        <p v-else-if="error" class="drive-home-error">{{ error }}</p>
        <NTable v-else class="drive-table" :bordered="false" :single-line="true" size="small">
          <thead>
            <tr>
              <th>文件名</th>
              <th class="drive-col-size">大小</th>
              <th class="drive-col-type">类型</th>
              <th class="drive-col-time">修改时间</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="s in storages"
              :key="s.id"
              class="drive-row"
              :class="{ 'drive-row--disabled': s.disabled }"
              tabindex="0"
              :aria-label="s.mount_path"
              @click="openStorage(s)"
              @keydown.enter="openStorage(s)"
            >
              <td>
                <span class="drive-name">
                  <NIcon :size="17" aria-hidden="true" class="drive-name-icon">
                    <FolderOpenOutline v-if="!s.disabled" />
                    <CloudOutline v-else />
                  </NIcon>
                  <span class="drive-name-text">{{ s.mount_path }}</span>
                  <span v-if="s.disabled" class="drive-name-badge">已禁用</span>
                </span>
              </td>
              <td class="drive-col-size drive-muted">—</td>
              <td class="drive-col-type drive-muted">文件夹</td>
              <td class="drive-col-time drive-muted">{{ fmtTime(s.modified) }}</td>
            </tr>
          </tbody>
        </NTable>
      </NSpin>
    </div>
  </div>
</template>

<style scoped>
.drive-home {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
}
.drive-home-toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  flex: none;
  margin-bottom: 12px;
}
.drive-home-hint {
  margin: 0;
  font-size: 13px;
  color: var(--m3-on-surface-variant);
}
.drive-home-list {
  flex: 1;
  min-height: 0;
  overflow: auto;
}
.drive-home-empty {
  padding: 48px 0;
}
.drive-home-error {
  padding: 16px;
  color: var(--m3-error, #d32f2f);
  font-size: 13px;
}
.drive-table {
  --n-th-color: transparent;
  --n-td-color: transparent;
  --n-border-color: transparent;
}
.drive-row {
  cursor: pointer;
}
.drive-row:focus-visible {
  outline: 2px solid var(--m3-primary);
  outline-offset: -2px;
}
.drive-row--disabled {
  opacity: 0.55;
  cursor: default;
}
.drive-row--disabled:hover td {
  background: transparent !important;
}
.drive-name {
  display: inline-flex;
  align-items: center;
  gap: 8px;
}
.drive-name-icon {
  color: var(--m3-on-surface-variant);
}
.drive-name-text {
  font-size: 14px;
  color: var(--m3-on-surface);
}
.drive-name-badge {
  margin-left: 8px;
  padding: 1px 6px;
  border-radius: 999px;
  font-size: 11px;
  color: var(--m3-on-surface-variant);
  background: rgba(128, 128, 128, 0.18);
}
.drive-muted {
  color: var(--m3-on-surface-variant);
  font-size: 13px;
}
.drive-col-size {
  width: 100px;
}
.drive-col-type {
  width: 110px;
}
.drive-col-time {
  width: 170px;
}
</style>
