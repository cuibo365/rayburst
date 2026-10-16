<script setup lang="ts">
/**
 * Drive explorer (首页): a single unified file-list page.
 * - Root level: all mounted storages rendered as folder rows (same table style).
 * - Storage level: browse a storage from its addition.root_path.
 * Breadcrumb: 首页 / <storage> / <subdirs...>, so the top nav always has a
 * "首页" level. Download is only available inside a storage.
 */
import { computed, onMounted, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NButton, NCheckbox, NEmpty, NIcon, NSpin, useMessage } from 'naive-ui'
import {
  ChevronForwardOutline,
  DocumentOutline,
  DownloadOutline,
  FolderOpenOutline,
  HomeOutline,
  RefreshOutline,
} from '@vicons/ionicons5'

interface DriveFile {
  fs_id: number
  name: string
  size: number
  path: string
  isdir: boolean
  mtime: number
  category: number
  /** Universal string id: Baidu decimal fs_id; Quark 32-char hex fid. */
  id: string
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

const message = useMessage()

// --- state ---
const storages = ref<CloudStorage[]>([])
const activeStorage = ref<CloudStorage | null>(null)
const rootPath = ref('/')
/** Browse stack: one entry per folder opened below the storage root.
 *  `key` is what the driver expects (Baidu: absolute path; Quark: fid). */
const browseStack = ref<{ name: string; key: string }[]>([])
const crumbSegs = computed(() => browseStack.value.map((x) => x.name))
const files = ref<DriveFile[]>([])
const loading = ref(false)
const loadError = ref('')
const selected = ref<Set<string>>(new Set())
const downloading = ref(false)
/** Item id currently being submitted by its row button, to disable just that row. */
const downloadingId = ref<string | null>(null)

const isRoot = computed(() => activeStorage.value === null)
const currentPath = computed(() => {
  if (isRoot.value) return '/'
  if (crumbSegs.value.length === 0) return rootPath.value
  return (rootPath.value === '/' ? '' : rootPath.value) + '/' + crumbSegs.value.join('/')
})
/** The browse key of the folder currently shown (root or last stack entry). */
const currentKey = computed(() => {
  if (browseStack.value.length === 0) return rootPath.value
  return browseStack.value[browseStack.value.length - 1].key
})
const selectedFiles = computed(() => files.value.filter((f) => selected.value.has(f.id)))
/** Only enabled storages are browsable, so disabled ones are hidden here.
 *  (The storage manager page still lists them so they can be re-enabled.) */
const visibleStorages = computed(() => storages.value.filter((s) => !s.disabled))

// --- formatting ---
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

function fmtDbTime(raw: string): string {
  const d = new Date(raw.includes('T') ? raw : raw.replace(' ', 'T') + 'Z')
  if (Number.isNaN(d.getTime())) return raw
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`
}

function errMsg(e: unknown): string {
  if (e instanceof Error) return e.message || '操作失败'
  if (typeof e === 'string' && e) return e
  if (e && typeof e === 'object') {
    const m = (e as Record<string, unknown>).message
    if (typeof m === 'string' && m) return m
    // Tauri serializes AppError enums as `{ "Variant": "detail" }` (or nested
    // struct variants), so fall back to the first field when there is no
    // `message` key.
    const first = Object.values(e as Record<string, unknown>)[0]
    if (typeof first === 'string' && first) return first
    if (first && typeof first === 'object') {
      const fm = (first as Record<string, unknown>).message
      if (typeof fm === 'string' && fm) return fm
    }
    try {
      return JSON.stringify(e)
    } catch {
      /* ignore */
    }
  }
  return '操作失败，请查看日志'
}

// --- db readiness (cold-start race guard) ---
async function ensureDbReady(): Promise<boolean> {
  for (let i = 0; i < 20; i++) {
    try {
      await invoke<number>('database_schema_version')
      return true
    } catch {
      await new Promise((r) => setTimeout(r, 200))
    }
  }
  return false
}

// --- root level: all storages ---
async function loadStorages() {
  loading.value = true
  loadError.value = ''
  try {
    storages.value = await invoke<CloudStorage[]>('cloud_storage_list')
  } catch (err) {
    loadError.value = errMsg(err)
    storages.value = []
  } finally {
    loading.value = false
  }
}

function enterStorage(s: CloudStorage) {
  if (s.disabled) return
  activeStorage.value = s
  // Fid-based drivers (Quark drive / Quark share) start at the fid root `0`;
  // Baidu starts at the mount root path.
  const fidBased = s.driver === 'quark' || s.driver === 'quark_share'
  let root = fidBased ? '0' : '/'
  try {
    const addition = JSON.parse(s.addition) as { root_path?: string; root_id?: string }
    root = s.driver === 'quark' ? addition.root_id?.trim() || '0' : addition.root_path?.trim() || '/'
  } catch {
    // keep driver default above
  }
  rootPath.value = root
  browseStack.value = []
  void loadDir(root)
}

// --- storage level: browse a folder ---
async function loadDir(fullPath: string) {
  if (!activeStorage.value) return
  loading.value = true
  loadError.value = ''
  try {
    const list = await invoke<DriveFile[]>('cloud_drive_list_path', {
      storageId: activeStorage.value.id,
      path: fullPath,
    })
    files.value = list
    selected.value = new Set()
  } catch (err) {
    loadError.value = errMsg(err)
    files.value = []
    selected.value = new Set()
  } finally {
    loading.value = false
  }
}

function goHome() {
  if (isRoot.value) return
  activeStorage.value = null
  browseStack.value = []
  files.value = []
  selected.value = new Set()
  void loadStorages()
}

function goToStorageRoot() {
  if (!activeStorage.value) return
  browseStack.value = []
  void loadDir(rootPath.value)
}

function goToCrumb(i: number) {
  // i = -1 -> storage root; 0.. -> that stack entry
  if (i < 0) {
    browseStack.value = []
    void loadDir(rootPath.value)
    return
  }
  browseStack.value = browseStack.value.slice(0, i + 1)
  void loadDir(browseStack.value[i].key)
}

function enterDir(f: DriveFile) {
  if (f.isdir) {
    browseStack.value.push({ name: f.name, key: f.path })
    void loadDir(f.path)
  }
}

function toggle(f: DriveFile) {
  const s = new Set(selected.value)
  if (s.has(f.id)) s.delete(f.id)
  else s.add(f.id)
  selected.value = s
}

async function refresh() {
  if (isRoot.value) await loadStorages()
  else await loadDir(currentKey.value)
}

async function submitDownload(list: DriveFile[], successMsg: (n: number) => string) {
  const r = (await invoke('cloud_drive_storage_download', {
    storageId: activeStorage.value!.id,
    files: list,
    currentDir: currentPath.value,
  })) as { message?: string }
  message.success(r?.message || successMsg(list.length))
}

async function startDownload() {
  const list = selectedFiles.value
  if (!list.length || !activeStorage.value) {
    message.warning('请先勾选要下载的文件')
    return
  }
  downloading.value = true
  try {
    await submitDownload(list, (n) => `已添加 ${n} 个文件到下载队列`)
  } catch (err) {
    message.error(errMsg(err))
  } finally {
    downloading.value = false
  }
}

/** Download a single row (folder contents are expanded by the backend). */
async function downloadOne(f: DriveFile) {
  if (!activeStorage.value || downloadingId.value !== null) return
  downloadingId.value = f.id
  try {
    await submitDownload([f], () => `已添加「${f.name}」到下载队列`)
  } catch (err) {
    message.error(errMsg(err))
  } finally {
    downloadingId.value = null
  }
}

onMounted(async () => {
  const ready = await ensureDbReady()
  if (!ready) {
    loadError.value = '数据库尚未就绪，请稍后刷新重试'
    return
  }
  void loadStorages()
})
</script>

<template>
  <div class="drive-explorer">
    <div class="drive-toolbar">
      <div class="drive-crumbs">
        <button type="button" class="drive-crumb" :class="{ 'drive-crumb--current': isRoot }" @click="goHome">
          <NIcon :size="14" aria-hidden="true" class="drive-crumb-home-icon"><HomeOutline /></NIcon>
          首页
        </button>
        <template v-if="activeStorage">
          <NIcon :size="14" class="drive-crumb-sep" aria-hidden="true"><ChevronForwardOutline /></NIcon>
          <button
            type="button"
            class="drive-crumb"
            :class="{ 'drive-crumb--current': crumbSegs.length === 0 }"
            @click="goToStorageRoot"
          >
            {{ activeStorage.mount_path }}
          </button>
          <template v-for="(seg, i) in crumbSegs" :key="seg + i">
            <NIcon :size="14" class="drive-crumb-sep" aria-hidden="true"><ChevronForwardOutline /></NIcon>
            <button
              type="button"
              class="drive-crumb"
              :class="{ 'drive-crumb--current': i === crumbSegs.length - 1 }"
              @click="goToCrumb(i)"
            >
              {{ seg }}
            </button>
          </template>
        </template>
      </div>
      <div class="drive-toolbar-actions">
        <NButton size="small" secondary @click="refresh">
          <template #icon
            ><NIcon><RefreshOutline /></NIcon
          ></template>
          刷新
        </NButton>
        <NButton
          v-if="!isRoot"
          size="small"
          type="primary"
          :disabled="!selectedFiles.length"
          :loading="downloading"
          @click="startDownload"
        >
          <template #icon
            ><NIcon><DownloadOutline /></NIcon
          ></template>
          批量下载<span v-if="selectedFiles.length">（{{ selectedFiles.length }}）</span>
        </NButton>
      </div>
    </div>

    <div class="drive-list">
      <NSpin :show="loading">
        <NEmpty
          v-if="!loading && isRoot && visibleStorages.length === 0"
          description="暂无可用存储，请到「存储」页添加或启用"
          class="drive-empty"
        />
        <NEmpty v-else-if="!loading && loadError" :description="loadError" class="drive-empty" />
        <NEmpty v-else-if="!loading && !isRoot && files.length === 0" description="此目录为空" class="drive-empty" />
        <table v-else class="drive-table">
          <thead>
            <tr>
              <th class="drive-col-check"></th>
              <th>文件名</th>
              <th class="drive-col-size">大小</th>
              <th class="drive-col-type">类型</th>
              <th class="drive-col-time">修改时间</th>
              <th v-if="!isRoot" class="drive-col-action">操作</th>
            </tr>
          </thead>
          <tbody v-if="isRoot">
            <tr
              v-for="s in visibleStorages"
              :key="s.id"
              class="drive-row"
              tabindex="0"
              :aria-label="s.mount_path"
              @click="enterStorage(s)"
              @keydown.enter="enterStorage(s)"
            >
              <td class="drive-col-check"></td>
              <td>
                <span class="drive-name drive-name--dir">
                  <NIcon :size="17" aria-hidden="true" class="drive-name-icon">
                    <FolderOpenOutline />
                  </NIcon>
                  <span class="drive-name-text">{{ s.mount_path }}</span>
                </span>
              </td>
              <td class="drive-col-size drive-muted">—</td>
              <td class="drive-col-type drive-muted">文件夹</td>
              <td class="drive-col-time drive-muted">{{ fmtDbTime(s.modified) }}</td>
            </tr>
          </tbody>
          <tbody v-else>
            <tr v-for="f in files" :key="f.id" class="drive-row" :class="{ 'drive-row--sel': selected.has(f.id) }">
              <td class="drive-col-check">
                <NCheckbox :checked="selected.has(f.id)" @update:checked="toggle(f)" />
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
              <td class="drive-col-action">
                <NButton
                  size="tiny"
                  secondary
                  :loading="downloadingId === f.id"
                  :disabled="downloadingId !== null && downloadingId !== f.id"
                  @click="downloadOne(f)"
                >
                  <template #icon
                    ><NIcon><DownloadOutline /></NIcon
                  ></template>
                  下载
                </NButton>
              </td>
            </tr>
          </tbody>
        </table>
      </NSpin>
    </div>
  </div>
</template>

<style scoped>
.drive-explorer {
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
  display: inline-flex;
  align-items: center;
  gap: 4px;
}
.drive-crumb:hover {
  background: rgba(128, 128, 128, 0.15);
}
.drive-crumb--current {
  color: var(--m3-primary, #3b82f6);
  font-weight: 600;
}
.drive-crumb-home-icon {
  color: var(--m3-on-surface-variant);
}
.drive-crumb-sep {
  color: var(--m3-on-surface-variant);
  flex: none;
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
.drive-row {
  cursor: pointer;
}
.drive-row:focus-visible {
  outline: 2px solid var(--m3-primary);
  outline-offset: -2px;
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
.drive-col-action {
  width: 84px;
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
