<script setup lang="ts">
/**
 * Storage browser: generic file list view for a mounted storage
 * (adapted from the legacy BaiduDriveApp list UI). Commands are
 * storage-scoped (`cloud_drive_list_path` / `cloud_drive_storage_download`),
 * and the browse root is the storage's addition.root_path.
 */
import { computed, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { invoke } from '@tauri-apps/api/core'
import { NButton, NCheckbox, NEmpty, NIcon, NSpin, useMessage } from 'naive-ui'
import {
  ChevronBackOutline,
  ChevronForwardOutline,
  DocumentOutline,
  DownloadOutline,
  FolderOpenOutline,
} from '@vicons/ionicons5'

interface DriveFile {
  fs_id: number
  name: string
  size: number
  path: string
  isdir: boolean
  mtime: number
  category: number
}

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

const props = defineProps<{ storageId: string }>()
const router = useRouter()
const message = useMessage()

const storage = ref<CloudStorage | null>(null)
const rootPath = ref('/')
const crumbPath = ref<string[]>([])
const files = ref<DriveFile[]>([])
const loading = ref(false)
const selected = ref<Set<number>>(new Set())
const downloading = ref(false)
const loadError = ref('')

const currentPath = computed(() => {
  if (crumbPath.value.length === 0) return rootPath.value
  return (rootPath.value === '/' ? '' : rootPath.value) + '/' + crumbPath.value.join('/')
})
const selectedFiles = computed(() => files.value.filter((f) => selected.value.has(f.fs_id)))

function fmtSize(n: number): string {
  if (!n) return '-'
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`
}

const CATEGORY_LABELS: Record<number, string> = {
  1: '视频',
  2: '音频',
  3: '图片',
  4: '文档',
  5: '应用',
  6: '其他',
  7: '种子',
}

function fmtType(f: DriveFile): string {
  if (f.isdir) return '文件夹'
  return CATEGORY_LABELS[f.category] ?? '文件'
}

function fmtTime(ts: number): string {
  if (!ts) return '-'
  const d = new Date(ts * 1000)
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`
}

function errMsg(e: unknown): string {
  if (e instanceof Error) return e.message || '操作失败'
  if (typeof e === 'string' && e) return e
  if (e && typeof e === 'object') {
    const m = (e as Record<string, unknown>).message
    if (typeof m === 'string' && m) return m
    try {
      return JSON.stringify(e)
    } catch {
      /* ignore */
    }
  }
  return '操作失败，请查看日志'
}

async function loadDir(fullPath: string) {
  loading.value = true
  loadError.value = ''
  try {
    const list = await invoke<DriveFile[]>('cloud_drive_list_path', {
      storageId: Number(props.storageId),
      path: fullPath,
    })
    files.value = list
    selected.value = new Set()
    const rel =
      fullPath === '/' ? '' : fullPath.replace(rootPath.value === '/' ? '/' : rootPath.value, '').replace(/^\/+/, '')
    crumbPath.value = rel ? rel.split('/').filter(Boolean) : []
  } catch (err) {
    const msg = errMsg(err)
    loadError.value = msg
    files.value = []
    selected.value = new Set()
  } finally {
    loading.value = false
  }
}

function goToCrumb(i: number) {
  const target =
    i < 0
      ? rootPath.value
      : (rootPath.value === '/' ? '' : rootPath.value) + '/' + crumbPath.value.slice(0, i + 1).join('/')
  void loadDir(target)
}

function enterDir(f: DriveFile) {
  if (f.isdir) void loadDir(f.path)
}

function toggle(f: DriveFile) {
  const s = new Set(selected.value)
  if (s.has(f.fs_id)) s.delete(f.fs_id)
  else s.add(f.fs_id)
  selected.value = s
}

async function startDownload() {
  const list = selectedFiles.value
  if (!list.length) {
    message.warning('请先勾选要下载的文件')
    return
  }
  downloading.value = true
  try {
    const r = (await invoke('cloud_drive_storage_download', {
      storageId: Number(props.storageId),
      files: list,
      currentDir: currentPath.value,
    })) as { message?: string }
    message.success(r?.message || `已添加 ${list.length} 个文件到下载队列`)
  } catch (err) {
    message.error(errMsg(err))
  } finally {
    downloading.value = false
  }
}

function goBack() {
  void router.push({ name: 'cloud-drive-home' })
}

onMounted(async () => {
  try {
    const all = await invoke<CloudStorage[]>('cloud_storage_list')
    const found = all.find((s) => String(s.id) === props.storageId)
    if (!found) {
      loadError.value = '存储不存在或已被删除'
      return
    }
    storage.value = found
    let root = '/'
    try {
      const addition = JSON.parse(found.addition) as { root_path?: string }
      root = addition.root_path?.trim() || '/'
    } catch {
      root = '/'
    }
    rootPath.value = root
    if (found.disabled) {
      loadError.value = '该存储已被禁用，请到「存储」页启用'
      return
    }
    await loadDir(root)
  } catch (err) {
    loadError.value = errMsg(err)
  }
})
</script>

<template>
  <div class="drive-browser">
    <div class="drive-toolbar">
      <NButton size="small" secondary @click="goBack">
        <template #icon
          ><NIcon><ChevronBackOutline /></NIcon
        ></template>
        返回
      </NButton>
      <div class="drive-crumbs">
        <button
          type="button"
          class="drive-crumb"
          :class="{ 'drive-crumb--root': crumbPath.length === 0 }"
          @click="goToCrumb(-1)"
        >
          {{ storage?.mount_path ?? '网盘' }}
        </button>
        <template v-for="(seg, i) in crumbPath" :key="seg + i">
          <NIcon :size="14" class="drive-crumb-sep" aria-hidden="true"><ChevronForwardOutline /></NIcon>
          <button
            type="button"
            class="drive-crumb"
            :class="{ 'drive-crumb--current': i === crumbPath.length - 1 }"
            @click="goToCrumb(i)"
          >
            {{ seg }}
          </button>
        </template>
      </div>
      <div class="drive-toolbar-actions">
        <NButton
          size="small"
          type="primary"
          :disabled="!selectedFiles.length"
          :loading="downloading"
          @click="startDownload"
        >
          <template #icon
            ><NIcon><DownloadOutline /></NIcon
          ></template>
          下载到 Rayburst{{ selectedFiles.length ? `（${selectedFiles.length}）` : '' }}
        </NButton>
      </div>
    </div>

    <div class="drive-list">
      <NSpin :show="loading">
        <NEmpty v-if="!loading && loadError" :description="loadError" class="drive-empty">
          <template #extra>
            <NButton size="small" secondary @click="goBack">返回存储列表</NButton>
          </template>
        </NEmpty>
        <NEmpty v-else-if="!loading && files.length === 0" description="此目录为空" class="drive-empty" />
        <table v-else class="drive-table">
          <thead>
            <tr>
              <th class="drive-col-check"></th>
              <th>名称</th>
              <th class="drive-col-size">大小</th>
              <th class="drive-col-type">类型</th>
              <th class="drive-col-time">修改时间</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="f in files"
              :key="f.fs_id"
              class="drive-row"
              :class="{ 'drive-row--sel': selected.has(f.fs_id) }"
            >
              <td class="drive-col-check">
                <NCheckbox :checked="selected.has(f.fs_id)" @update:checked="toggle(f)" />
              </td>
              <td>
                <button type="button" class="drive-name" :class="{ 'drive-name--dir': f.isdir }" @click="enterDir(f)">
                  <NIcon :size="17" aria-hidden="true" class="drive-name-icon">
                    <FolderOpenOutline v-if="f.isdir" />
                    <DocumentOutline v-else />
                  </NIcon>
                  <span class="drive-name-text">{{ f.name }}</span>
                </button>
              </td>
              <td class="drive-col-size drive-muted">{{ f.isdir ? '—' : fmtSize(f.size) }}</td>
              <td class="drive-col-type drive-muted">{{ fmtType(f) }}</td>
              <td class="drive-col-time drive-muted">{{ fmtTime(f.mtime) }}</td>
            </tr>
          </tbody>
        </table>
      </NSpin>
    </div>
  </div>
</template>

<style scoped>
.drive-browser {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.drive-toolbar {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}
.drive-crumbs {
  display: flex;
  align-items: center;
  gap: 2px;
  min-width: 0;
  overflow: hidden;
  flex-wrap: wrap;
  flex: 1;
}
.drive-crumb {
  border: none;
  background: transparent;
  color: var(--m3-on-surface-variant);
  font-size: 13px;
  cursor: pointer;
  padding: 4px 8px;
  border-radius: 6px;
  max-width: 160px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.drive-crumb:hover {
  background: rgba(128, 128, 128, 0.15);
}
.drive-crumb--root {
  color: var(--m3-on-surface);
  font-weight: 600;
}
.drive-crumb--current {
  color: var(--m3-primary, #3b82f6);
}
.drive-crumb-sep {
  color: var(--m3-on-surface-variant);
}
.drive-toolbar-actions {
  display: flex;
  gap: 8px;
}
.drive-list {
  flex: 1;
  min-height: 0;
  overflow: auto;
  border-radius: 10px;
  border: 1px solid rgba(128, 128, 128, 0.18);
}
.drive-empty {
  padding: 60px 0;
}
.drive-table {
  width: 100%;
  border-collapse: collapse;
  table-layout: fixed;
}
.drive-table th {
  text-align: left;
  font-size: 12px;
  font-weight: 600;
  color: var(--m3-on-surface-variant);
  padding: 8px 12px;
  border-bottom: 1px solid rgba(128, 128, 128, 0.18);
  background: rgba(128, 128, 128, 0.06);
}
.drive-table td {
  padding: 7px 12px;
  border-bottom: 1px solid rgba(128, 128, 128, 0.1);
}
.drive-row--sel {
  background: rgba(59, 130, 246, 0.08);
}
.drive-col-check {
  width: 44px;
}
.drive-col-size {
  width: 110px;
  text-align: right;
}
.drive-col-type {
  width: 90px;
  text-align: right;
}
.drive-col-time {
  width: 150px;
  text-align: right;
}
.drive-name {
  border: none;
  background: transparent;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 0;
  cursor: pointer;
  color: var(--m3-on-surface);
  font-size: 14px;
  width: 100%;
  text-align: left;
}
.drive-name--dir .drive-name-text {
  font-weight: 600;
}
.drive-name:hover .drive-name-text {
  text-decoration: underline;
}
.drive-name-icon {
  flex: none;
  color: var(--m3-on-surface-variant);
}
.drive-name--dir .drive-name-icon {
  color: #f0a020;
}
.drive-muted {
  color: var(--m3-on-surface-variant);
  font-size: 13px;
}
</style>
