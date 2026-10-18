<script setup lang="ts">
/**
 * Storage manager (存储): alist-style storage CRUD over drivers.
 * Adding a Baidu Netdisk storage fills client_id/client_secret with defaults;
 * the refresh_token field carries an "获取" button that opens the OAuth page.
 */
import { computed, onMounted, onUnmounted, ref } from 'vue'
import {
  NButton,
  NCheckbox,
  NForm,
  NFormItem,
  NIcon,
  NInput,
  NModal,
  NPopconfirm,
  NSelect,
  NSpace,
  NSpin,
  NTable,
  useMessage,
} from 'naive-ui'
import { AddOutline, CreateOutline, RefreshOutline, TrashOutline } from '@vicons/ionicons5'
import { invoke } from '@tauri-apps/api/core'
import { openUrl } from '@tauri-apps/plugin-opener'

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

const DEFAULT_CLIENT_ID = 'hq9yQ9w9kR4YHj1kyYafLygVocobh7Sf'
const DEFAULT_CLIENT_SECRET = 'YH2VpZcFJHYNnV6vLfHQXDBhcE7ZChyE'
const CALLBACK_URL = 'https://alistgo.com/tool/baidu/callback'

function errMsg(e: unknown): string {
  if (e instanceof Error) return e.message || '操作失败'
  if (typeof e === 'string' && e) return e
  if (e && typeof e === 'object') {
    const m = (e as Record<string, unknown>).message
    if (typeof m === 'string' && m) return m
    // Tauri serializes AppError enums as `{ "Variant": "detail" }`; pull the
    // first value so users see the actual reason instead of "[object Object]".
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

const DRIVERS = [
  { label: '百度网盘', value: 'baidu_netdisk' },
  { label: '夸克网盘', value: 'quark' },
  { label: '夸克分享', value: 'quark_share' },
  { label: '115 网盘', value: 'm115' },
  { label: '115 分享', value: 'm115_share' },
  { label: '阿里云盘', value: 'aliyun_drive' },
  { label: '阿里分享', value: 'aliyun_share' },
  { label: '阿里云盘转115', value: 'aliyun_drive_115' },
  { label: '阿里分享转115', value: 'aliyun_share_115' },
] as const

const message = useMessage()
const storages = ref<CloudStorage[]>([])
const loading = ref(false)

// --- add / edit dialog ---
const showDialog = ref(false)
const saving = ref(false)
const editingId = ref<number | null>(null)
const form = ref<{
  mount_path: string
  driver: string
  refresh_token: string
  client_id: string
  client_secret: string
  root_path: string
  root_folder_id: string
  oauth_token_url: string
  cookie: string
  root_id: string
  share_url: string
  share_code: string
  passcode: string
  receive_code: string
  user_key: string
  dir_id: string
  open_copy_dir: string
  remark: string
}>({
  mount_path: '',
  driver: 'baidu_netdisk',
  refresh_token: '',
  client_id: '',
  client_secret: '',
  root_path: '/',
  root_folder_id: 'root',
  oauth_token_url: 'https://api.alistgo.com/alist/ali_open/token',
  cookie: '',
  root_id: '0',
  share_url: '',
  share_code: '',
  passcode: '',
  receive_code: '',
  user_key: '',
  dir_id: '0',
  open_copy_dir: 'root',
  remark: '',
})

const dialogTitle = computed(() => (editingId.value === null ? '添加存储' : '编辑存储'))
const isBaidu = computed(() => form.value.driver === 'baidu_netdisk')
const isQuark = computed(() => form.value.driver === 'quark')
const isQuarkShare = computed(() => form.value.driver === 'quark_share')
const isM115 = computed(() => form.value.driver === 'm115')
const isM115Share = computed(() => form.value.driver === 'm115_share')
const isAliyunDrive = computed(() => form.value.driver === 'aliyun_drive')
const isAliyunShare = computed(() => form.value.driver === 'aliyun_share')
const isAliyunDrive115 = computed(() => form.value.driver === 'aliyun_drive_115')
const isAliyunShare115 = computed(() => form.value.driver === 'aliyun_share_115')

function resetForm() {
  editingId.value = null
  form.value = {
    mount_path: '',
    driver: 'baidu_netdisk',
    refresh_token: '',
    client_id: '',
    client_secret: '',
    root_path: '/',
    root_folder_id: 'root',
    oauth_token_url: 'https://api.alistgo.com/alist/ali_open/token',
    cookie: '',
    root_id: '0',
    share_url: '',
    share_code: '',
    passcode: '',
    receive_code: '',
    user_key: '',
    dir_id: '0',
    open_copy_dir: 'root',
    remark: '',
  }
}

function openAdd() {
  resetForm()
  showDialog.value = true
}

function openEdit(s: CloudStorage) {
  editingId.value = s.id
  let addition: Record<string, unknown> = {}
  try {
    addition = JSON.parse(s.addition) as Record<string, unknown>
  } catch {
    addition = {}
  }
  form.value = {
    mount_path: s.mount_path,
    driver: s.driver,
    refresh_token: (addition.refresh_token as string) ?? '',
    client_id:
      s.driver.startsWith('aliyun') && (addition.client_id as string) === DEFAULT_CLIENT_ID
        ? ''
        : ((addition.client_id as string) ?? ''),
    client_secret:
      s.driver.startsWith('aliyun') && (addition.client_secret as string) === DEFAULT_CLIENT_SECRET
        ? ''
        : ((addition.client_secret as string) ?? ''),
    root_path: (addition.root_path as string) ?? '/',
    root_folder_id: (addition.root_folder_id as string) ?? 'root',
    oauth_token_url: (addition.oauth_token_url as string) ?? 'https://api.alistgo.com/alist/ali_open/token',
    cookie: (addition.cookie as string) ?? '',
    root_id: (addition.root_id as string) ?? '0',
    share_url: (addition.share_url as string) ?? '',
    share_code: (addition.share_code as string) ?? '',
    passcode: (addition.passcode as string) ?? '',
    receive_code: (addition.receive_code as string) ?? '',
    user_key: (addition.user_key as string) ?? '',
    dir_id: (addition.dir_id as string) ?? '0',
    open_copy_dir: (addition.open_copy_dir as string) ?? 'root',
    remark: s.remark,
  }
  showDialog.value = true
}

function onDriverChange() {
  // Re-apply driver defaults whenever the driver switches (or is first picked).
  form.value.client_id = ''
  form.value.client_secret = ''
  form.value.root_path = '/'
  form.value.root_folder_id = 'root'
  form.value.oauth_token_url = 'https://api.alistgo.com/alist/ali_open/token'
  form.value.cookie = ''
  form.value.root_id = '0'
  form.value.share_url = ''
  form.value.passcode = ''
  form.value.receive_code = ''
  form.value.user_key = ''
  form.value.dir_id = '0'
  form.value.open_copy_dir = 'root'
}

// --- qrcode login (扫码获取 Cookie): quark / 115 ---
const qrDialog = ref(false)
const qrKind = ref<'quark' | 'm115'>('quark')
const qrSvg = ref('')
const qrToken = ref('')
const qrUid = ref('')
const qrTime = ref(0)
const qrSign = ref('')
const qrError = ref('')
const qrGenerating = ref(false)
let qrTimer: number | null = null

function stopQrPoll() {
  if (qrTimer !== null) {
    window.clearInterval(qrTimer)
    qrTimer = null
  }
}

async function genQrcode() {
  stopQrPoll()
  qrError.value = ''
  qrGenerating.value = true
  try {
    if (qrKind.value === 'm115') {
      const r = await invoke<{ qrcode_svg: string; uid: string; time: number; sign: string }>(
        'cloud_drive_115_qrcode_create',
      )
      qrUid.value = r.uid
      qrTime.value = r.time
      qrSign.value = r.sign
      qrSvg.value = r.qrcode_svg
      qrTimer = window.setInterval(async () => {
        if (!qrUid.value) return
        try {
          const r = await invoke<{ state: string; cookie?: string; message?: string }>(
            'cloud_drive_115_qrcode_status',
            { uid: qrUid.value, time: qrTime.value, sign: qrSign.value },
          )
          if (r.state === 'success' && r.cookie) {
            form.value.cookie = r.cookie
            stopQrPoll()
            qrDialog.value = false
            message.success('扫码成功，Cookie 已自动填入')
          } else if (r.state === 'expired' || r.state === 'canceled' || r.state === 'error') {
            stopQrPoll()
            qrError.value = r.message || '扫码失败，请刷新二维码重试'
          }
        } catch {
          // transient network error: keep polling
        }
      }, 2000)
    } else {
      const r = await invoke<{ token: string; qrcode_svg: string }>('cloud_drive_quark_qrcode_create')
      qrToken.value = r.token
      qrSvg.value = r.qrcode_svg
      qrTimer = window.setInterval(async () => {
        if (!qrToken.value) return
        try {
          const r = await invoke<{ state: string; cookie?: string; message?: string }>(
            'cloud_drive_quark_qrcode_status',
            { token: qrToken.value },
          )
          if (r.state === 'success' && r.cookie) {
            form.value.cookie = r.cookie
            stopQrPoll()
            qrDialog.value = false
            message.success('扫码成功，Cookie 已自动填入')
          } else if (r.state === 'expired' || r.state === 'error') {
            stopQrPoll()
            qrError.value = r.message || '扫码失败，请刷新二维码重试'
          }
        } catch {
          // transient network error: keep polling
        }
      }, 2000)
    }
  } catch (err) {
    qrError.value = errMsg(err)
  } finally {
    qrGenerating.value = false
  }
}

function openQrcode(kind: 'quark' | 'm115' = 'quark') {
  qrKind.value = kind
  qrSvg.value = ''
  qrToken.value = ''
  qrUid.value = ''
  qrTime.value = 0
  qrSign.value = ''
  qrError.value = ''
  qrDialog.value = true
  void genQrcode()
}

onUnmounted(stopQrPoll)

async function openAuthorize() {
  const clientId = form.value.client_id.trim() || DEFAULT_CLIENT_ID
  const url = `https://openapi.baidu.com/oauth/2.0/authorize?response_type=code&client_id=${encodeURIComponent(clientId)}&redirect_uri=${encodeURIComponent(CALLBACK_URL)}&scope=basic,netdisk&qrcode=1`
  try {
    await openUrl(url)
  } catch (e) {
    message.error(`打开授权页面失败：${String(e)}`)
  }
}

function buildAddition(): Record<string, unknown> {
  if (form.value.driver === 'quark') {
    return {
      cookie: form.value.cookie.trim(),
      root_id: form.value.root_id.trim() || '0',
    }
  }
  if (form.value.driver === 'quark_share') {
    return {
      share_url: form.value.share_url.trim(),
      cookie: form.value.cookie.trim(),
      passcode: form.value.passcode.trim(),
    }
  }
  if (form.value.driver === 'm115') {
    return {
      cookie: form.value.cookie.trim(),
      root_id: form.value.root_id.trim() || '0',
    }
  }
  if (form.value.driver === 'm115_share') {
    return {
      share_url: form.value.share_url.trim(),
      share_code: form.value.share_code.trim(),
      receive_code: form.value.receive_code.trim(),
      cookie: form.value.cookie.trim(),
    }
  }
  if (form.value.driver === 'aliyun_drive') {
    return {
      refresh_token: form.value.refresh_token.trim(),
      client_id: form.value.client_id.trim(),
      client_secret: form.value.client_secret.trim(),
      oauth_token_url: form.value.oauth_token_url.trim(),
      root_folder_id: form.value.root_folder_id.trim() || 'root',
    }
  }
  if (form.value.driver === 'aliyun_share') {
    return {
      refresh_token: form.value.refresh_token.trim(),
      share_url: form.value.share_url.trim(),
      share_code: form.value.share_code.trim(),
      share_pwd: form.value.passcode.trim(),
    }
  }
  if (form.value.driver === 'aliyun_drive_115') {
    return {
      refresh_token: form.value.refresh_token.trim(),
      client_id: form.value.client_id.trim(),
      client_secret: form.value.client_secret.trim(),
      oauth_token_url: form.value.oauth_token_url.trim(),
      root_folder_id: form.value.root_folder_id.trim() || 'root',
      cookie: form.value.cookie.trim(),
      user_key: form.value.user_key.trim(),
      dir_id: form.value.dir_id.trim() || '0',
    }
  }
  if (form.value.driver === 'aliyun_share_115') {
    return {
      refresh_token: form.value.refresh_token.trim(),
      client_id: form.value.client_id.trim(),
      client_secret: form.value.client_secret.trim(),
      oauth_token_url: form.value.oauth_token_url.trim(),
      open_copy_dir: form.value.open_copy_dir.trim() || 'root',
      share_url: form.value.share_url.trim(),
      share_code: form.value.share_code.trim(),
      share_pwd: form.value.passcode.trim(),
      cookie: form.value.cookie.trim(),
      user_key: form.value.user_key.trim(),
      dir_id: form.value.dir_id.trim() || '0',
    }
  }
  return {
    refresh_token: form.value.refresh_token.trim(),
    root_path: form.value.root_path.trim() || '/',
    client_id: form.value.client_id.trim() || DEFAULT_CLIENT_ID,
    client_secret: form.value.client_secret.trim() || DEFAULT_CLIENT_SECRET,
  }
}

async function save() {
  if (!form.value.mount_path.trim()) {
    message.warning('请填写挂载路径')
    return
  }
  if (form.value.driver === 'baidu_netdisk' && !form.value.refresh_token.trim()) {
    message.warning('请填写 refresh_token（可点击「获取」打开授权页面）')
    return
  }
  if (form.value.driver === 'quark' && !form.value.cookie.trim()) {
    message.warning('请填写夸克网页版 Cookie（登录 pan.quark.cn 后从浏览器复制）')
    return
  }
  if (form.value.driver === 'quark_share') {
    if (!form.value.share_url.trim()) {
      message.warning('请填写夸克分享地址（https://pan.quark.cn/s/...）')
      return
    }
    if (!form.value.cookie.trim()) {
      message.warning('请填写夸克网页版 Cookie（下载分享文件需要登录态）')
      return
    }
  }
  if (form.value.driver === 'm115' && !form.value.cookie.trim()) {
    message.warning('请填写 115 网页版 Cookie（登录 115.com 后从浏览器复制）')
    return
  }
  if (form.value.driver === 'm115_share') {
    if (!form.value.share_url.trim() && !form.value.share_code.trim()) {
      message.warning('请填写 115 分享地址（https://115.com/s/xxxx 或 115cdn.com/s/xxxx）')
      return
    }
  }
  if (form.value.driver === 'aliyun_drive' && !form.value.refresh_token.trim()) {
    message.warning('请填写阿里云盘 refresh_token')
    return
  }
  if (form.value.driver === 'aliyun_share') {
    if (!form.value.share_url.trim()) {
      message.warning('请填写阿里分享地址（https://www.alipan.com/s/xxxx）')
      return
    }
    if (!form.value.refresh_token.trim()) {
      message.warning('请填写阿里云盘 refresh_token')
      return
    }
  }
  if (form.value.driver === 'aliyun_drive_115') {
    if (!form.value.refresh_token.trim()) {
      message.warning('请填写阿里云盘 refresh_token')
      return
    }
    if (!form.value.cookie.trim()) {
      message.warning('请填写 115 Cookie（闪传需要登录态）')
      return
    }
  }
  if (form.value.driver === 'aliyun_share_115') {
    if (!form.value.share_url.trim()) {
      message.warning('请填写阿里分享地址（https://www.alipan.com/s/xxxx）')
      return
    }
    if (!form.value.refresh_token.trim()) {
      message.warning('请填写阿里云盘 refresh_token（分享文件需先转存到你的阿里云盘）')
      return
    }
    if (!form.value.cookie.trim()) {
      message.warning('请填写 115 Cookie（闪传需要登录态）')
      return
    }
  }
  saving.value = true
  try {
    const addition = buildAddition()
    if (editingId.value === null) {
      await invoke('cloud_storage_add', {
        mountPath: form.value.mount_path.trim(),
        driver: form.value.driver,
        addition: JSON.stringify(addition),
        remark: form.value.remark.trim(),
      })
      message.success('存储已添加')
    } else {
      await invoke('cloud_storage_update', {
        id: editingId.value,
        mountPath: form.value.mount_path.trim(),
        addition: JSON.stringify(addition),
        remark: form.value.remark.trim(),
      })
      message.success('存储已更新')
    }
    showDialog.value = false
    await load()
  } catch (e) {
    message.error(errMsg(e))
  } finally {
    saving.value = false
  }
}

async function toggleDisabled(s: CloudStorage) {
  try {
    await invoke('cloud_storage_toggle', { id: s.id, disabled: !s.disabled })
    await load()
  } catch (e) {
    message.error(errMsg(e))
  }
}

async function remove(s: CloudStorage) {
  try {
    await invoke('cloud_storage_delete', { id: s.id })
    message.success('存储已删除')
    await load()
  } catch (e) {
    message.error(errMsg(e))
  }
}

function fmtTime(raw: string): string {
  const d = new Date(raw.includes('T') ? raw : raw.replace(' ', 'T') + 'Z')
  if (Number.isNaN(d.getTime())) return raw
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`
}

async function load() {
  loading.value = true
  try {
    storages.value = await invoke<CloudStorage[]>('cloud_storage_list')
  } catch (e) {
    message.error(errMsg(e))
  } finally {
    loading.value = false
  }
}

onMounted(async () => {
  if (await ensureDbReady()) await load()
  else message.error('数据库尚未就绪，请稍后刷新重试')
})
</script>

<template>
  <div class="storage-manager">
    <div class="storage-toolbar">
      <p class="storage-hint">共 {{ storages.length }} 个存储</p>
      <NSpace>
        <NButton size="small" secondary @click="load">
          <template #icon
            ><NIcon><RefreshOutline /></NIcon
          ></template>
          刷新
        </NButton>
        <NButton size="small" type="primary" @click="openAdd">
          <template #icon
            ><NIcon><AddOutline /></NIcon
          ></template>
          添加存储
        </NButton>
      </NSpace>
    </div>

    <div class="storage-list">
      <NSpin :show="loading">
        <NTable class="storage-table" :bordered="false" :single-line="true" size="small">
          <thead>
            <tr>
              <th>挂载路径</th>
              <th class="storage-col-driver">驱动</th>
              <th>状态</th>
              <th>备注</th>
              <th class="storage-col-disabled">禁用</th>
              <th class="storage-col-time">修改时间</th>
              <th class="storage-col-ops">操作</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="s in storages" :key="s.id">
              <td class="storage-mount">{{ s.mount_path }}</td>
              <td class="storage-col-driver storage-muted">
                {{
                  s.driver === 'baidu_netdisk'
                    ? '百度网盘'
                    : s.driver === 'quark'
                      ? '夸克网盘'
                      : s.driver === 'quark_share'
                        ? '夸克分享'
                        : s.driver === 'm115'
                          ? '115 网盘'
                          : s.driver === 'm115_share'
                            ? '115 分享'
                            : s.driver === 'aliyun_drive'
                              ? '阿里云盘'
                              : s.driver === 'aliyun_share'
                                ? '阿里分享'
                                : s.driver === 'aliyun_drive_115'
                                  ? '阿里云盘转115'
                                  : s.driver === 'aliyun_share_115'
                                    ? '阿里分享转115'
                                    : s.driver
                }}
              </td>
              <td>
                <span class="storage-status" :class="{ 'storage-status--err': s.status !== '正常' }">{{
                  s.status
                }}</span>
              </td>
              <td class="storage-remark storage-muted">{{ s.remark || '—' }}</td>
              <td class="storage-col-disabled">
                <NCheckbox
                  :checked="s.disabled"
                  :aria-label="`禁用 ${s.mount_path}`"
                  @update:checked="toggleDisabled(s)"
                />
              </td>
              <td class="storage-col-time storage-muted">{{ fmtTime(s.modified) }}</td>
              <td class="storage-col-ops">
                <NSpace :size="4">
                  <NButton size="tiny" secondary @click="openEdit(s)">
                    <template #icon
                      ><NIcon><CreateOutline /></NIcon
                    ></template>
                    编辑
                  </NButton>
                  <NPopconfirm @positive-click="remove(s)">
                    <template #trigger>
                      <NButton size="tiny" secondary type="error">
                        <template #icon
                          ><NIcon><TrashOutline /></NIcon
                        ></template>
                        删除
                      </NButton>
                    </template>
                    删除存储「{{ s.mount_path }}」？此操作不可撤销。
                  </NPopconfirm>
                </NSpace>
              </td>
            </tr>
            <tr v-if="storages.length === 0 && !loading">
              <td colspan="7" class="storage-empty">暂无存储，点击「添加存储」开始</td>
            </tr>
          </tbody>
        </NTable>
      </NSpin>
    </div>

    <!-- 添加 / 编辑 dialog -->
    <NModal
      v-model:show="showDialog"
      preset="card"
      :title="dialogTitle"
      class="storage-dialog"
      :style="{ width: '520px' }"
      :bordered="false"
    >
      <NForm label-placement="top">
        <NFormItem label="挂载路径">
          <NInput v-model:value="form.mount_path" placeholder="必填，如：百度网盘" />
        </NFormItem>
        <NFormItem label="驱动">
          <NSelect v-model:value="form.driver" :options="[...DRIVERS]" @update:value="onDriverChange" />
        </NFormItem>

        <template v-if="isBaidu">
          <NFormItem label="refresh_token">
            <template #label>
              <div class="storage-cookie-label">
                <span>refresh_token</span>
                <NButton size="small" type="primary" @click="openAuthorize">扫码获取</NButton>
              </div>
            </template>
            <NInput
              v-model:value="form.refresh_token"
              type="textarea"
              :autosize="{ minRows: 2, maxRows: 4 }"
              placeholder="粘贴 refresh_token（以 122. 开头）"
            />
          </NFormItem>
          <NSpace :size="12">
            <NFormItem label="client_id" class="storage-flex-item">
              <NInput v-model:value="form.client_id" />
            </NFormItem>
            <NFormItem label="client_secret" class="storage-flex-item">
              <NInput v-model:value="form.client_secret" />
            </NFormItem>
          </NSpace>
          <NFormItem label="root_path（挂载根目录）">
            <NInput v-model:value="form.root_path" placeholder="/" />
          </NFormItem>
        </template>

        <template v-else-if="isQuark">
          <NFormItem label="Cookie">
            <template #label>
              <div class="storage-cookie-label">
                <span>Cookie</span>
                <NButton size="small" type="primary" @click="openQrcode('quark')">扫码获取</NButton>
              </div>
            </template>
            <NInput
              v-model:value="form.cookie"
              type="textarea"
              :autosize="{ minRows: 4, maxRows: 8 }"
              placeholder="登录 pan.quark.cn 后，按 F12 → Application → Cookies 复制全部 Cookie 粘贴到这里"
            />
          </NFormItem>
          <p class="storage-quark-tip">
            浏览器登录 <code>pan.quark.cn</code> → F12 → Application → Cookies 全选复制（形如
            <code>xxx=yyy; zzz=aaa</code>）；或点右上角「扫码获取」用夸克 App 自动获取。
          </p>
          <NFormItem label="root_id（挂载根目录 fid）">
            <NInput v-model:value="form.root_id" placeholder="0 = 网盘根目录" />
          </NFormItem>
        </template>

        <template v-else-if="isQuarkShare">
          <NFormItem label="分享地址">
            <NInput v-model:value="form.share_url" placeholder="https://pan.quark.cn/s/xxxx（可带 ?pwd= 提取码）" />
          </NFormItem>
          <NFormItem label="提取码（非必填）">
            <NInput v-model:value="form.passcode" placeholder="分享链接未带 pwd 时使用此提取码" />
          </NFormItem>
          <NFormItem label="Cookie">
            <template #label>
              <div class="storage-cookie-label">
                <span>Cookie</span>
                <NButton size="small" type="primary" @click="openQrcode('quark')">扫码获取</NButton>
              </div>
            </template>
            <NInput
              v-model:value="form.cookie"
              type="textarea"
              :autosize="{ minRows: 4, maxRows: 8 }"
              placeholder="登录 pan.quark.cn 后，按 F12 → Application → Cookies 复制全部 Cookie 粘贴到这里"
            />
          </NFormItem>
          <p class="storage-quark-tip">
            分享浏览无需登录，但<strong>下载需要 Cookie</strong>。浏览器登录 <code>pan.quark.cn</code> → F12 →
            Application → Cookies 全选复制，或点右上角「扫码获取」用夸克 App 自动获取。
          </p>
        </template>

        <template v-else-if="isM115">
          <NFormItem label="Cookie">
            <template #label>
              <div class="storage-cookie-label">
                <span>Cookie</span>
                <NButton size="small" type="primary" @click="openQrcode('m115')">扫码获取</NButton>
              </div>
            </template>
            <NInput
              v-model:value="form.cookie"
              type="textarea"
              :autosize="{ minRows: 4, maxRows: 8 }"
              placeholder="登录 115.com 后，按 F12 → Application → Cookies 复制（需含 UID/CID/SEID）"
            />
          </NFormItem>
          <p class="storage-quark-tip">
            浏览器登录 <code>115.com</code> → F12 → Application → Cookies 全选复制（形如
            <code>UID=xxx; CID=yyy; SEID=zzz</code>）；或点右上角「扫码获取」用 115 App 自动获取。
          </p>
          <NFormItem label="root_id（挂载根目录 cid）">
            <NInput v-model:value="form.root_id" placeholder="0 = 网盘根目录" />
          </NFormItem>
        </template>

        <template v-else-if="isM115Share">
          <NFormItem label="分享地址">
            <NInput
              v-model:value="form.share_url"
              placeholder="https://115.com/s/xxxx?password=提取码（分享码自动解析）"
            />
          </NFormItem>
          <NFormItem label="提取码（非必填）">
            <NInput v-model:value="form.receive_code" placeholder="分享链接未带 password 时使用此提取码" />
          </NFormItem>
          <NFormItem label="Cookie">
            <template #label>
              <div class="storage-cookie-label">
                <span>Cookie</span>
                <NButton size="small" type="primary" @click="openQrcode('m115')">扫码获取</NButton>
              </div>
            </template>
            <NInput
              v-model:value="form.cookie"
              type="textarea"
              :autosize="{ minRows: 4, maxRows: 8 }"
              placeholder="登录 115.com 后，按 F12 → Application → Cookies 复制（需含 UID/CID/SEID）"
            />
          </NFormItem>
          <p class="storage-quark-tip">
            分享浏览无需登录，但<strong>下载需要 Cookie</strong>。浏览器登录 <code>115.com</code> → F12 → Application →
            Cookies 全选复制（形如 <code>UID=xxx; CID=yyy; SEID=zzz</code>），或点右上角「扫码获取」用 115 App
            自动获取。
          </p>
        </template>

        <template v-else-if="isAliyunDrive || isAliyunDrive115">
          <NFormItem label="阿里云盘 refresh_token">
            <NInput
              v-model:value="form.refresh_token"
              type="textarea"
              :autosize="{ minRows: 2, maxRows: 4 }"
              placeholder="粘贴阿里云盘开放平台 refresh_token（网页授权后获取，形如 1A2B... 较长）"
            />
          </NFormItem>
          <p class="storage-quark-tip">
            <strong>获取方式：</strong>参考 alist「阿里云盘 Open」——在
            <code>https://openapi.alipan.com/oauth/authorize</code> 授权后拿 refresh_token（也可用第三方托管 alist
            扫码获取）。此处暂未内置扫码，请手动粘贴。
          </p>
          <NSpace :size="12">
            <NFormItem label="client_id" class="storage-flex-item">
              <NInput v-model:value="form.client_id" placeholder="默认 alist 官方 client_id" />
            </NFormItem>
            <NFormItem label="client_secret" class="storage-flex-item">
              <NInput v-model:value="form.client_secret" placeholder="默认 alist 官方 client_secret" />
            </NFormItem>
          </NSpace>
          <NFormItem label="oauth_token_url（自建托管时填写）">
            <NInput v-model:value="form.oauth_token_url" placeholder="https://api.alistgo.com/alist/ali_open/token" />
          </NFormItem>
          <NFormItem v-if="isAliyunDrive" label="root_folder_id（挂载根目录）">
            <NInput v-model:value="form.root_folder_id" placeholder="root = 网盘根目录，也可填具体文件夹 file_id" />
          </NFormItem>
          <template v-if="isAliyunDrive115">
            <NFormItem label="115 Cookie">
              <NInput
                v-model:value="form.cookie"
                type="textarea"
                :autosize="{ minRows: 3, maxRows: 6 }"
                placeholder="登录 115.com 后复制（需含 UID/CID/SEID）"
              />
            </NFormItem>
            <NFormItem label="dir_id（闪传落盘目录 cid）">
              <NInput
                v-model:value="form.dir_id"
                placeholder="0 = 网盘根目录（下载的文件会以时间戳后缀转存到该目录）"
              />
            </NFormItem>
            <NFormItem label="user_key（非必填）">
              <NInput v-model:value="form.user_key" placeholder="115 用户名/手机号，留空自动从 Cookie 解析" />
            </NFormItem>
            <p class="storage-quark-tip">
              下载流程：阿里文件 SHA1 → 115 闪传（秒传命中即取 pickcode）→ 生成 115 CDN 直链。闪传是上传通道，
              <strong>不消耗 115 云下载配额</strong>；文件会出现在你的 115 网盘「最近接受」，属 115 全局行为。
            </p>
          </template>
        </template>

        <template v-else-if="isAliyunShare || isAliyunShare115">
          <NFormItem label="分享地址">
            <NInput v-model:value="form.share_url" placeholder="https://www.alipan.com/s/xxxx（可带 ?pwd= 提取码）" />
          </NFormItem>
          <NFormItem label="提取码（非必填）">
            <NInput v-model:value="form.passcode" placeholder="分享链接未带 pwd 时使用此提取码" />
          </NFormItem>
          <NFormItem label="阿里云盘 refresh_token">
            <NInput
              v-model:value="form.refresh_token"
              type="textarea"
              :autosize="{ minRows: 2, maxRows: 4 }"
              placeholder="粘贴你自己的阿里云盘 refresh_token（分享文件需转存到你的盘）"
            />
          </NFormItem>
          <p class="storage-quark-tip">
            <strong>注意：</strong>分享文件无 SHA1，下载前会先 <code>v2/file/copy</code> 转存到你自己的阿里云盘
            （默认根目录，见下方「转存目录」），再走 SHA1 → 115 闪传 → 直链。
          </p>
          <template v-if="isAliyunShare115">
            <NSpace :size="12">
              <NFormItem label="client_id" class="storage-flex-item">
                <NInput v-model:value="form.client_id" placeholder="默认 alist 官方 client_id" />
              </NFormItem>
              <NFormItem label="client_secret" class="storage-flex-item">
                <NInput v-model:value="form.client_secret" placeholder="默认 alist 官方 client_secret" />
              </NFormItem>
            </NSpace>
            <NFormItem label="oauth_token_url（自建托管时填写）">
              <NInput v-model:value="form.oauth_token_url" placeholder="https://api.alistgo.com/alist/ali_open/token" />
            </NFormItem>
            <NFormItem label="转存目录（open_copy_dir）">
              <NInput v-model:value="form.open_copy_dir" placeholder="root = 网盘根目录，也可填文件夹 file_id" />
            </NFormItem>
            <NFormItem label="115 Cookie">
              <NInput
                v-model:value="form.cookie"
                type="textarea"
                :autosize="{ minRows: 3, maxRows: 6 }"
                placeholder="登录 115.com 后复制（需含 UID/CID/SEID）"
              />
            </NFormItem>
            <NFormItem label="dir_id（闪传落盘目录 cid）">
              <NInput
                v-model:value="form.dir_id"
                placeholder="0 = 网盘根目录（下载的文件会以时间戳后缀转存到该目录）"
              />
            </NFormItem>
            <NFormItem label="user_key（非必填）">
              <NInput v-model:value="form.user_key" placeholder="115 用户名/手机号，留空自动从 Cookie 解析" />
            </NFormItem>
            <p class="storage-quark-tip">
              下载流程：分享文件 → 转存到你阿里云盘 → SHA1 → 115 闪传（不耗云下载配额）→ 115 CDN 直链。文件会出现在
              115「最近接受」。
            </p>
          </template>
        </template>

        <NFormItem label="备注">
          <NInput v-model:value="form.remark" placeholder="可选" />
        </NFormItem>
      </NForm>
      <template #footer>
        <div class="storage-dialog-actions">
          <NButton secondary @click="showDialog = false">取消</NButton>
          <NButton type="primary" :loading="saving" @click="save">保存</NButton>
        </div>
      </template>
    </NModal>

    <!-- 扫码获取 Cookie（夸克 / 115） -->
    <NModal
      v-model:show="qrDialog"
      preset="card"
      :title="qrKind === 'm115' ? '115 扫码获取 Cookie' : '夸克扫码获取 Cookie'"
      class="storage-dialog"
      :style="{ width: '360px' }"
      :bordered="false"
      @after-leave="stopQrPoll"
    >
      <p class="storage-quark-tip">
        <template v-if="qrKind === 'm115'">
          请使用<strong>115 App</strong> 扫码登录（手机端需已登录账号），扫码后请在手机上确认登录，成功后自动填入
          Cookie。
        </template>
        <template v-else>
          请使用<strong>夸克 App</strong> 扫码登录（手机端需已登录账号），扫码成功后将自动填入 Cookie。
        </template>
      </p>
      <div class="qr-box">
        <img
          v-if="qrSvg"
          :src="'data:image/svg+xml;utf8,' + encodeURIComponent(qrSvg)"
          :alt="qrKind === 'm115' ? '115 登录二维码' : '夸克登录二维码'"
        />
        <NSpin v-else :show="qrGenerating" class="qr-loading" />
      </div>
      <div v-if="qrError" class="qr-error">{{ qrError }}</div>
      <template #footer>
        <div class="storage-dialog-actions">
          <NButton secondary @click="qrDialog = false">关闭</NButton>
          <NButton secondary :loading="qrGenerating" @click="genQrcode">刷新二维码</NButton>
        </div>
      </template>
    </NModal>
  </div>
</template>

<style scoped>
.storage-manager {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
}
.storage-toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  flex: none;
  margin-bottom: 12px;
}
.storage-hint {
  margin: 0;
  font-size: 13px;
  color: var(--m3-on-surface-variant);
}
.storage-list {
  flex: 1;
  min-height: 0;
  overflow: auto;
}
.storage-table {
  --n-th-color: transparent;
  --n-td-color: transparent;
  --n-border-color: transparent;
}
.storage-mount {
  font-size: 14px;
  color: var(--m3-on-surface);
}
.storage-muted {
  color: var(--m3-on-surface-variant);
  font-size: 13px;
}
.storage-status {
  font-size: 13px;
  color: var(--m3-primary);
}
.storage-status--err {
  color: var(--m3-error, #d32f2f);
}
.storage-remark {
  max-width: 160px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.storage-empty {
  text-align: center;
  padding: 40px 0;
  color: var(--m3-on-surface-variant);
  font-size: 13px;
}
.storage-col-driver {
  width: 120px;
}
.storage-col-disabled {
  width: 70px;
  text-align: center;
}
.storage-col-time {
  width: 150px;
}
.storage-col-ops {
  width: 140px;
  white-space: nowrap;
}
.storage-cookie-label {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  width: 100%;
}
.qr-box {
  display: flex;
  justify-content: center;
  align-items: center;
  min-height: 240px;
  padding: 12px 0;
}
.qr-box img {
  width: 240px;
  height: 240px;
  image-rendering: pixelated;
  border-radius: 8px;
  background: #fff;
}
.qr-loading {
  padding: 90px 0;
}
.qr-error {
  font-size: 12px;
  color: var(--m3-error, #e5484d);
  margin-top: 4px;
}
.storage-quark-tip {
  margin: -4px 0 10px;
  font-size: 12px;
  line-height: 1.6;
  color: var(--m3-on-surface-variant);
}
.storage-quark-tip code {
  background: rgba(127, 127, 127, 0.12);
  border-radius: 3px;
  padding: 0 3px;
  font-size: 11px;
}
.storage-flex-item {
  flex: 1;
}
.storage-dialog-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
</style>

<!--
  NModal teleports its content to <body>, so scoped styles can never reach the
  dialog's inner DOM. These global rules are pinned to .storage-dialog to make
  the 扫码获取 buttons sit flush right in the form label row.
-->
<style>
.storage-dialog .n-form-item-label {
  display: flex;
  align-items: center;
  width: 100%;
}
.storage-dialog .n-form-item-label__text {
  display: flex;
  align-items: center;
  width: 100%;
}
</style>
