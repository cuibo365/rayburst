<script setup lang="ts">
/**
 * Self-hosted Baidu Netdisk UI (应用内网盘).
 *
 * No Baidu web page is embedded: the Rust backend talks to Baidu's APIs
 * (QR login -> stored BDUSS -> xpan list -> dlink -> rayrburst aria2), and
 * this component renders the drive UI natively.
 */
import { onMounted, ref, computed } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { openUrl } from '@tauri-apps/plugin-opener'
import { NIcon, NButton, NEmpty, NSpin, NCheckbox, NInput } from 'naive-ui'
import {
  FolderOpenOutline,
  DocumentOutline,
  DownloadOutline,
  LogOutOutline,
  ChevronForwardOutline,
  CloudOutline,
  QrCodeOutline,
} from '@vicons/ionicons5'
import { useAppMessage } from '@/composables/useAppMessage'

interface DriveFile {
  fs_id: number
  name: string
  size: number
  path: string
  isdir: boolean
  mtime: number
  category: number
}

const message = useAppMessage()

const state = ref<'checking' | 'login' | 'browse'>('checking')
const authInput = ref('')
const saving = ref(false)
const authorizeUrl = ref('')

const crumbPath = ref<string[]>([])
const files = ref<DriveFile[]>([])
const loading = ref(false)
const selected = ref<Set<number>>(new Set())
const downloading = ref(false)

const currentPath = computed(() => (crumbPath.value.length ? '/' + crumbPath.value.join('/') : '/'))
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

async function openAuthorize() {
  try {
    if (!authorizeUrl.value) {
      authorizeUrl.value = await invoke<string>('cloud_drive_baidu_oauth_url')
    }
    await openUrl(authorizeUrl.value)
    message.info('请在浏览器中完成授权，然后复制页面中的 code 或 access_token 粘贴到下方')
  } catch (err) {
    message.error(errMsg(err))
  }
}

async function saveToken() {
  const input = authInput.value.trim()
  if (!input) {
    message.warning('请先粘贴 code 或 access_token')
    return
  }
  saving.value = true
  try {
    const r = (await invoke('cloud_drive_baidu_save_token', {
      tokenOrCode: input,
    })) as string
    message.success(r)
    authInput.value = ''
    state.value = 'browse'
    await loadDir('/')
  } catch (err) {
    message.error(errMsg(err))
  } finally {
    saving.value = false
  }
}

async function loadDir(dir: string) {
  loading.value = true
  try {
    const list = await invoke<DriveFile[]>('cloud_drive_baidu_list', { dir })
    files.value = list
    selected.value = new Set()
    crumbPath.value = dir === '/' ? [] : dir.split('/').filter(Boolean)
  } catch (err) {
    const msg = errMsg(err)
    if (/未授权|未登录|access_token/.test(msg)) {
      state.value = 'login'
    } else {
      message.error(msg)
    }
  } finally {
    loading.value = false
  }
}

function goToCrumb(i: number) {
  const target = i < 0 ? '/' : '/' + crumbPath.value.slice(0, i + 1).join('/')
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
    const r = (await invoke('cloud_drive_baidu_download', {
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

function logout() {
  void invoke('cloud_drive_baidu_logout').catch(() => {})
  state.value = 'login'
}

onMounted(async () => {
  try {
    const logged = await invoke<boolean>('cloud_drive_baidu_login_status')
    if (logged) {
      state.value = 'browse'
      await loadDir('/')
    } else {
      state.value = 'login'
    }
  } catch {
    state.value = 'login'
  }
})
</script>

<template>
  <div class="baidu-app">
    <!-- 登录：二维码 -->
    <div v-if="state === 'checking'" class="baidu-center">
      <NSpin size="large" />
      <p class="baidu-muted">正在检查登录状态…</p>
    </div>

    <div v-else-if="state === 'login'" class="baidu-center">
      <div class="baidu-login-card">
        <div class="baidu-login-title">
          <NIcon :size="22" aria-hidden="true"><CloudOutline /></NIcon>
          <span>授权百度网盘</span>
        </div>
        <p class="baidu-muted">
          点击下方按钮在浏览器中打开授权页面（支持扫码登录），完成授权后复制页面中的
          <b>refresh_token</b> 粘贴到下方输入框保存，将自动换取 access_token。
        </p>
        <div class="baidu-login-actions">
          <NButton type="primary" block @click="openAuthorize">
            <template #icon
              ><NIcon><QrCodeOutline /></NIcon
            ></template>
            打开授权页面
          </NButton>
        </div>
        <NInput
          v-model:value="authInput"
          type="textarea"
          :autosize="{ minRows: 2, maxRows: 4 }"
          placeholder="粘贴 refresh_token（以 122. 开头），将自动换取 access_token"
          class="baidu-login-input"
        />
        <NButton type="primary" block :loading="saving" :disabled="!authInput.trim()" @click="saveToken">
          保存并登录
        </NButton>
      </div>
    </div>

    <!-- 浏览：文件列表 -->
    <div v-else class="baidu-browse">
      <div class="baidu-toolbar">
        <div class="baidu-crumbs">
          <button
            type="button"
            class="baidu-crumb"
            :class="{ 'baidu-crumb--root': crumbPath.length === 0 }"
            @click="goToCrumb(-1)"
          >
            我的网盘
          </button>
          <template v-for="(seg, i) in crumbPath" :key="seg + i">
            <NIcon :size="14" class="baidu-crumb-sep" aria-hidden="true"><ChevronForwardOutline /></NIcon>
            <button
              type="button"
              class="baidu-crumb"
              :class="{ 'baidu-crumb--current': i === crumbPath.length - 1 }"
              @click="goToCrumb(i)"
            >
              {{ seg }}
            </button>
          </template>
        </div>
        <div class="baidu-toolbar-actions">
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
          <NButton size="small" secondary @click="logout">
            <template #icon
              ><NIcon><LogOutOutline /></NIcon
            ></template>
            退出登录
          </NButton>
        </div>
      </div>

      <div class="baidu-list">
        <NSpin :show="loading">
          <NEmpty v-if="!loading && files.length === 0" description="此目录为空" class="baidu-empty" />
          <table v-else class="baidu-table">
            <thead>
              <tr>
                <th class="baidu-col-check"></th>
                <th>名称</th>
                <th class="baidu-col-size">大小</th>
                <th class="baidu-col-type">类型</th>
                <th class="baidu-col-time">修改时间</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="f in files"
                :key="f.fs_id"
                class="baidu-row"
                :class="{ 'baidu-row--sel': selected.has(f.fs_id) }"
              >
                <td class="baidu-col-check">
                  <NCheckbox :checked="selected.has(f.fs_id)" @update:checked="toggle(f)" />
                </td>
                <td>
                  <button type="button" class="baidu-name" :class="{ 'baidu-name--dir': f.isdir }" @click="enterDir(f)">
                    <NIcon :size="17" aria-hidden="true" class="baidu-name-icon">
                      <FolderOpenOutline v-if="f.isdir" />
                      <DocumentOutline v-else />
                    </NIcon>
                    <span class="baidu-name-text">{{ f.name }}</span>
                  </button>
                </td>
                <td class="baidu-col-size baidu-muted">{{ f.isdir ? '—' : fmtSize(f.size) }}</td>
                <td class="baidu-col-type baidu-muted">{{ fmtType(f) }}</td>
                <td class="baidu-col-time baidu-muted">{{ fmtTime(f.mtime) }}</td>
              </tr>
            </tbody>
          </table>
        </NSpin>
      </div>
    </div>
  </div>
</template>

<style scoped>
.baidu-app {
  height: 100%;
  min-height: 0;
  display: flex;
  flex-direction: column;
  box-sizing: border-box;
}
.baidu-center {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 12px;
  padding: 24px;
}
.baidu-muted {
  margin: 0;
  font-size: 13px;
  color: var(--m3-on-surface-variant);
}
.baidu-login-card {
  width: 340px;
  padding: 24px;
  border-radius: 12px;
  background: var(--m3-surface-container, rgba(255, 255, 255, 0.06));
  border: 1px solid rgba(128, 128, 128, 0.25);
  display: flex;
  flex-direction: column;
  align-items: stretch;
  gap: 14px;
}
.baidu-login-title {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  font-size: 16px;
  font-weight: 600;
  color: var(--m3-on-surface);
}
.baidu-login-card .baidu-muted {
  text-align: left;
  line-height: 1.6;
}
.baidu-login-input {
  width: 100%;
}
.baidu-login-actions {
  display: flex;
  gap: 8px;
}

.baidu-browse {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.baidu-toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-wrap: wrap;
}
.baidu-crumbs {
  display: flex;
  align-items: center;
  gap: 2px;
  min-width: 0;
  overflow: hidden;
  flex-wrap: wrap;
}
.baidu-crumb {
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
.baidu-crumb:hover {
  background: rgba(128, 128, 128, 0.15);
}
.baidu-crumb--root {
  color: var(--m3-on-surface);
  font-weight: 600;
}
.baidu-crumb--current {
  color: var(--m3-primary, #3b82f6);
}
.baidu-crumb-sep {
  color: var(--m3-on-surface-variant);
}
.baidu-toolbar-actions {
  display: flex;
  gap: 8px;
}
.baidu-list {
  flex: 1;
  min-height: 0;
  overflow: auto;
  border-radius: 10px;
  border: 1px solid rgba(128, 128, 128, 0.18);
}
.baidu-empty {
  padding: 60px 0;
}
.baidu-table {
  width: 100%;
  border-collapse: collapse;
  table-layout: fixed;
}
.baidu-table th {
  text-align: left;
  font-size: 12px;
  font-weight: 600;
  color: var(--m3-on-surface-variant);
  padding: 8px 12px;
  border-bottom: 1px solid rgba(128, 128, 128, 0.18);
  background: rgba(128, 128, 128, 0.06);
}
.baidu-table td {
  padding: 7px 12px;
  border-bottom: 1px solid rgba(128, 128, 128, 0.1);
}
.baidu-row--sel {
  background: rgba(59, 130, 246, 0.08);
}
.baidu-col-check {
  width: 44px;
}
.baidu-col-size {
  width: 110px;
  text-align: right;
}
.baidu-col-type {
  width: 90px;
  text-align: right;
}
.baidu-col-time {
  width: 150px;
  text-align: right;
}
.baidu-name {
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
.baidu-name--dir .baidu-name-text {
  font-weight: 600;
}
.baidu-name:hover .baidu-name-text {
  text-decoration: underline;
}
.baidu-name-icon {
  flex: none;
  color: var(--m3-on-surface-variant);
}
.baidu-name--dir .baidu-name-icon {
  color: #f0a020;
}
</style>
