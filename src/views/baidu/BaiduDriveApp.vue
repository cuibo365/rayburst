<script setup lang="ts">
/**
 * Self-hosted Baidu Netdisk UI (应用内网盘).
 *
 * No Baidu web page is embedded: the Rust backend talks to Baidu's APIs
 * (QR login -> stored BDUSS -> xpan list -> dlink -> rayrburst aria2), and
 * this component renders the drive UI natively.
 */
import { onMounted, onUnmounted, ref, computed } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NIcon, NButton, NEmpty, NSpin, NCheckbox } from 'naive-ui'
import {
  FolderOpenOutline,
  DocumentOutline,
  DownloadOutline,
  RefreshOutline,
  LogOutOutline,
  ChevronForwardOutline,
  CloudOutline,
} from '@vicons/ionicons5'
import { useAppMessage } from '@/composables/useAppMessage'

interface DriveFile {
  fs_id: number
  name: string
  size: number
  path: string
  isdir: boolean
}
interface QrData {
  imgurl: string
  sign: string
}
interface QrStatus {
  status: string
  bduss?: string
}

const message = useAppMessage()

const state = ref<'checking' | 'login' | 'browse'>('checking')
const qr = ref<QrData | null>(null)
const qrExpired = ref(false)
const pollTimer = ref<number | null>(null)

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

function stopPoll() {
  if (pollTimer.value !== null) {
    clearInterval(pollTimer.value)
    pollTimer.value = null
  }
}

async function pollOnce() {
  if (!qr.value) return
  try {
    const st = await invoke<QrStatus>('cloud_drive_baidu_qrlogin', { sign: qr.value.sign })
    if (st.status === 'ok') {
      stopPoll()
      message.success('登录成功')
      state.value = 'browse'
      await loadDir('/')
    } else if (st.status === 'expired') {
      qrExpired.value = true
      stopPoll()
    }
  } catch {
    // transient network error; keep polling
  }
}

function startPoll() {
  stopPoll()
  pollTimer.value = window.setInterval(pollOnce, 3000)
}

async function refreshQr() {
  qrExpired.value = false
  try {
    qr.value = await invoke<QrData>('cloud_drive_baidu_qrcode')
    startPoll()
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err)
    message.error(msg || '获取二维码失败')
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
    const msg = err instanceof Error ? err.message : String(err)
    if (/未登录|登录/.test(msg)) {
      state.value = 'login'
      await refreshQr()
    } else {
      message.error(msg || '加载目录失败')
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
    const msg = err instanceof Error ? err.message : String(err)
    message.error(msg || '下载失败')
  } finally {
    downloading.value = false
  }
}

function logout() {
  stopPoll()
  void invoke('cloud_drive_baidu_logout').catch(() => {})
  state.value = 'login'
  void refreshQr()
}

onMounted(async () => {
  try {
    const logged = await invoke<boolean>('cloud_drive_baidu_login_status')
    if (logged) {
      state.value = 'browse'
      await loadDir('/')
    } else {
      state.value = 'login'
      await refreshQr()
    }
  } catch {
    state.value = 'login'
    await refreshQr()
  }
})

onUnmounted(stopPoll)
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
          <span>扫码登录百度网盘</span>
        </div>
        <p class="baidu-muted">使用手机百度 App / 百度网盘 App 扫码，登录后即可浏览与下载</p>
        <div class="baidu-qr">
          <img v-if="qr && qr.imgurl" :src="qr.imgurl" alt="登录二维码" class="baidu-qr-img" />
          <div v-else class="baidu-qr-empty"><NSpin size="large" /></div>
          <div v-if="qrExpired" class="baidu-qr-overlay">
            <NButton type="primary" size="small" @click="refreshQr">二维码已过期，点击刷新</NButton>
          </div>
        </div>
        <div class="baidu-login-actions">
          <NButton size="small" secondary @click="refreshQr">
            <template #icon
              ><NIcon><RefreshOutline /></NIcon
            ></template>
            刷新二维码
          </NButton>
        </div>
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
  width: 300px;
  padding: 24px;
  border-radius: 12px;
  background: var(--m3-surface-container, rgba(255, 255, 255, 0.06));
  border: 1px solid rgba(128, 128, 128, 0.25);
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 14px;
}
.baidu-login-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 16px;
  font-weight: 600;
  color: var(--m3-on-surface);
}
.baidu-qr {
  position: relative;
  width: 180px;
  height: 180px;
  border-radius: 8px;
  overflow: hidden;
  background: #fff;
  display: flex;
  align-items: center;
  justify-content: center;
}
.baidu-qr-img {
  width: 180px;
  height: 180px;
  display: block;
}
.baidu-qr-overlay {
  position: absolute;
  inset: 0;
  background: rgba(255, 255, 255, 0.92);
  display: flex;
  align-items: center;
  justify-content: center;
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
  width: 100px;
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
