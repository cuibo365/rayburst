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
  drive_type: string
  qr_channel: string
  cookie: string
  root_id: string
  share_url: string
  share_code: string
  passcode: string
  receive_code: string
  open_copy_dir: string
  own_drive_id: string
  qr_source: string
  remark: string
}>({
  mount_path: '',
  driver: 'baidu_netdisk',
  refresh_token: '',
  client_id: '',
  client_secret: '',
  root_path: '/',
  root_folder_id: 'root',
  drive_type: 'resource',
  qr_channel: 'messense',
  cookie: '',
  root_id: '0',
  share_url: '',
  share_code: '',
  passcode: '',
  receive_code: '',
  open_copy_dir: 'root',
  own_drive_id: '',
  qr_source: 'alipaymini',
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
    // 新增默认就是百度网盘：预填公共 client_id/client_secret（可手动改）
    client_id: DEFAULT_CLIENT_ID,
    client_secret: DEFAULT_CLIENT_SECRET,
    root_path: '/',
    root_folder_id: 'root',
    drive_type: 'resource',
    qr_channel: 'messense',
    cookie: '',
    root_id: '0',
    share_url: '',
    share_code: '',
    passcode: '',
    receive_code: '',
    open_copy_dir: 'root',
    own_drive_id: '',
    qr_source: 'alipaymini',
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
    drive_type: (addition.drive_type as string) ?? 'resource',
    qr_channel: (addition.qr_channel as string) ?? 'messense',
    cookie: (addition.cookie as string) ?? '',
    root_id: (addition.root_id as string) ?? '0',
    share_url: (addition.share_url as string) ?? '',
    share_code: (addition.share_code as string) ?? '',
    passcode: (addition.passcode as string) ?? '',
    receive_code: (addition.receive_code as string) ?? '',
    open_copy_dir: (addition.open_copy_dir as string) ?? 'root',
    own_drive_id: (addition.own_drive_id as string) ?? '',
    qr_source: (addition.qr_source as string) ?? 'alipaymini',
    remark: s.remark,
  }
  showDialog.value = true
}

// 凭据组：同组驱动/存储共享同一账号凭据，新增存储时可复用。
// 字段必须同源带（如阿里 open 的 refresh_token 与扫码通道需对应）。
interface CredentialGroup {
  key: string
  label: string
  drivers: string[]
  fields: string[]
}

const credentialGroups: CredentialGroup[] = [
  {
    key: 'aliyunOpen',
    label: '阿里 open 账号来源（自动带入 refresh_token + 扫码通道）',
    drivers: ['aliyun_drive', 'aliyun_drive_115'],
    fields: ['refresh_token', 'qr_channel'],
  },
  {
    key: 'aliyunShare',
    label: '阿里分享账号来源（自动带入 refresh_token）',
    drivers: ['aliyun_share', 'aliyun_share_115'],
    fields: ['refresh_token'],
  },
  {
    key: 'm115',
    label: '115 账号来源（自动带入 Cookie + 扫码设备）',
    drivers: ['m115', 'm115_share', 'aliyun_drive_115', 'aliyun_share_115'],
    fields: ['cookie', 'qr_source'],
  },
  {
    key: 'quark',
    label: '夸克账号来源（自动带入 Cookie）',
    drivers: ['quark', 'quark_share'],
    fields: ['cookie'],
  },
  {
    key: 'baidu',
    label: '百度账号来源（自动带入 refresh_token）',
    drivers: ['baidu_netdisk'],
    fields: ['refresh_token'],
  },
]

function credCandidates(g: CredentialGroup): CloudStorage[] {
  return storages.value
    .filter((s) => !s.disabled && g.drivers.includes(s.driver))
    .sort((a, b) => (a.created_at < b.created_at ? -1 : 1))
}

function credOptions(g: CredentialGroup) {
  return credCandidates(g).map((s) => ({
    label: `${s.mount_path}${s.remark ? ` · ${s.remark}` : ''}`,
    value: s.id,
  }))
}

function applyCredSource(g: CredentialGroup, storage: CloudStorage | null) {
  if (!storage) return
  let addition: Record<string, unknown> = {}
  try {
    addition = JSON.parse(storage.addition) as Record<string, unknown>
  } catch {
    addition = {}
  }
  for (const f of g.fields) {
    const v = addition[f]
    ;(form.value as Record<string, unknown>)[f] = typeof v === 'string' ? v : ''
  }
}

// 「同步获取」弹窗：选择组内来源存储，确认后填入对应字段。
const syncOpen = ref(false)
const syncGroup = ref<CredentialGroup | null>(null)
const syncSelected = ref<number | null>(null)
const syncOptions = computed(() => (syncGroup.value ? credOptions(syncGroup.value) : []))

function openSync(key: string) {
  const g = credentialGroups.find((x) => x.key === key)
  if (!g) return
  syncGroup.value = g
  syncSelected.value = null
  syncOpen.value = true
}

// 选中来源即填入对应字段并关闭；清空（null）不填、弹窗保持打开。
function onSyncPick(v: number | null) {
  if (v === null) return
  const g = syncGroup.value
  if (!g) return
  const s = storages.value.find((x) => x.id === v)
  if (s) applyCredSource(g, s)
  syncOpen.value = false
}

function onDriverChange() {
  // Re-apply driver defaults whenever the driver switches (or is first picked).
  form.value.refresh_token = ''
  form.value.client_id = ''
  form.value.client_secret = ''
  form.value.root_path = '/'
  form.value.root_folder_id = 'root'
  form.value.drive_type = 'resource'
  form.value.qr_channel = 'messense'
  form.value.cookie = ''
  form.value.root_id = '0'
  form.value.share_url = ''
  form.value.passcode = ''
  form.value.receive_code = ''
  form.value.open_copy_dir = 'root'
  form.value.qr_source = 'alipaymini'
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
// 扫码设备取自表单「扫码设备」记录值；未选时默认 115生活（支付宝小程序）。
const activeQrSource = () => form.value.qr_source?.trim() || 'alipaymini'
const m115QrSourceOptions = [
  { label: '115生活（支付宝小程序）', value: 'alipaymini' },
  { label: '115生活（网页端）', value: 'web' },
  { label: '115生活（iOS端）', value: 'ios' },
  { label: '115（iOS端）', value: '115ios' },
  { label: '115生活（Android端）', value: 'android' },
  { label: '115（Android端）', value: '115android' },
  { label: '115（iPad端）', value: '115ipad' },
  { label: '115生活（Android电视端）', value: 'tv' },
  { label: '115管理（Android端）', value: 'qandroid' },
  { label: '115生活（微信小程序）', value: 'wechatmini' },
  { label: '115（鸿蒙端）', value: 'harmony' },
]
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
        { source: activeQrSource() },
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
            {
              uid: qrUid.value,
              time: qrTime.value,
              sign: qrSign.value,
              source: activeQrSource(),
            },
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
onUnmounted(stopAliyunQrPoll)
onUnmounted(stopAliyunWebQrPoll)

async function openAuthorize() {
  const clientId = form.value.client_id.trim() || DEFAULT_CLIENT_ID
  const url = `https://openapi.baidu.com/oauth/2.0/authorize?response_type=code&client_id=${encodeURIComponent(clientId)}&redirect_uri=${encodeURIComponent(CALLBACK_URL)}&scope=basic,netdisk&qrcode=1`
  try {
    await openUrl(url)
  } catch (e) {
    message.error(`打开授权页面失败：${String(e)}`)
  }
}

/// Aliyun Drive refresh_token: jump to the alistgo official token page.
// --- Aliyun Drive open token QR login (xiaoya dual channel) ---// The channel (messense / xiaoya) determines which hosted OAuth issues the
// refresh_token; the channel is recorded into `qr_channel` so runtime
// refresh stays in-channel.
const aliyunQrDialog = ref(false)
const aliyunQrSvg = ref('')
const aliyunQrSid = ref('')
const aliyunQrError = ref('')
const aliyunQrGenerating = ref(false)
let aliyunQrTimer: number | null = null

function stopAliyunQrPoll() {
  if (aliyunQrTimer !== null) {
    window.clearInterval(aliyunQrTimer)
    aliyunQrTimer = null
  }
}

async function genAliyunQrcode() {
  stopAliyunQrPoll()
  aliyunQrError.value = ''
  aliyunQrGenerating.value = true
  try {
    const r = await invoke<{ sid: string; qr_code_url: string; qrcode_svg: string }>('aliyun_qr_create', {
      channel: form.value.qr_channel || 'messense',
    })
    aliyunQrSid.value = r.sid
    aliyunQrSvg.value = r.qrcode_svg
    aliyunQrTimer = window.setInterval(async () => {
      if (!aliyunQrSid.value) return
      try {
        const s = await invoke<{ state: string; auth_code?: string; message?: string }>('aliyun_qr_status', {
          sid: aliyunQrSid.value,
        })
        if (s.state === 'success' && s.auth_code) {
          const t = await invoke<{ refresh_token: string; token_url: string }>('aliyun_qr_exchange', {
            channel: form.value.qr_channel || 'messense',
            authCode: s.auth_code,
          })
          form.value.refresh_token = t.refresh_token
          stopAliyunQrPoll()
          aliyunQrDialog.value = false
          message.success('扫码成功，refresh_token 已自动填入')
        } else if (s.state === 'expired' || s.state === 'error') {
          stopAliyunQrPoll()
          aliyunQrError.value = s.message || '二维码已失效，请刷新重试'
        }
      } catch {
        // transient network error: keep polling
      }
    }, 2000)
  } catch (err) {
    aliyunQrError.value = errMsg(err)
  } finally {
    aliyunQrGenerating.value = false
  }
}

function openAliyunQr() {
  aliyunQrSvg.value = ''
  aliyunQrSid.value = ''
  aliyunQrError.value = ''
  aliyunQrDialog.value = true
  void genAliyunQrcode()
}

// --- Aliyun web-domain token QR login (alistgo official endpoints) ---
// Used by the share drivers (aliyun_share / aliyun_share_115): the web
// refresh_token differs from the open token and is obtained via alistgo's
// passport-based /alist/ali/qr + /alist/ali/ck.
const aliyunWebQrDialog = ref(false)
const aliyunWebQrSvg = ref('')
const aliyunWebQrCk = ref('')
const aliyunWebQrT = ref('')
const aliyunWebQrError = ref('')
const aliyunWebQrGenerating = ref(false)
let aliyunWebQrTimer: number | null = null

function stopAliyunWebQrPoll() {
  if (aliyunWebQrTimer !== null) {
    window.clearInterval(aliyunWebQrTimer)
    aliyunWebQrTimer = null
  }
}

async function genAliyunWebQrcode() {
  stopAliyunWebQrPoll()
  aliyunWebQrError.value = ''
  aliyunWebQrGenerating.value = true
  try {
    const r = await invoke<{ ck: string; t: string; qrcode_svg: string }>('aliyun_web_qr_create')
    aliyunWebQrCk.value = r.ck
    aliyunWebQrT.value = r.t
    aliyunWebQrSvg.value = r.qrcode_svg
    aliyunWebQrTimer = window.setInterval(async () => {
      if (!aliyunWebQrCk.value || !aliyunWebQrT.value) return
      try {
        const s = await invoke<{ state: string; refresh_token?: string; message?: string }>('aliyun_web_qr_status', {
          ck: aliyunWebQrCk.value,
          t: aliyunWebQrT.value,
        })
        if (s.state === 'success' && s.refresh_token) {
          form.value.refresh_token = s.refresh_token
          stopAliyunWebQrPoll()
          aliyunWebQrDialog.value = false
          message.success('扫码成功，refresh_token 已自动填入')
        } else if (s.state === 'expired' || s.state === 'error') {
          stopAliyunWebQrPoll()
          aliyunWebQrError.value = s.message || '二维码已失效，请刷新重试'
        }
      } catch {
        // transient network error: keep polling
      }
    }, 2000)
  } catch (err) {
    aliyunWebQrError.value = errMsg(err)
  } finally {
    aliyunWebQrGenerating.value = false
  }
}

function openAliyunWebQr() {
  aliyunWebQrSvg.value = ''
  aliyunWebQrCk.value = ''
  aliyunWebQrT.value = ''
  aliyunWebQrError.value = ''
  aliyunWebQrDialog.value = true
  void genAliyunWebQrcode()
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
      qr_source: form.value.qr_source.trim() || undefined,
    }
  }
  if (form.value.driver === 'm115_share') {
    return {
      share_url: form.value.share_url.trim(),
      share_code: form.value.share_code.trim(),
      receive_code: form.value.receive_code.trim(),
      cookie: form.value.cookie.trim(),
      qr_source: form.value.qr_source.trim() || undefined,
    }
  }
  if (form.value.driver === 'aliyun_drive') {
    return {
      refresh_token: form.value.refresh_token.trim(),
      qr_channel: form.value.qr_channel.trim(),
      root_folder_id: form.value.root_folder_id.trim() || 'root',
      drive_type: form.value.drive_type.trim() || 'resource',
    }
  }
  if (form.value.driver === 'aliyun_share') {
    return {
      refresh_token: form.value.refresh_token.trim(),
      share_url: form.value.share_url.trim(),
      share_code: form.value.share_code.trim(),
      share_pwd: form.value.passcode.trim(),
      open_copy_dir: form.value.open_copy_dir.trim() || 'root',
      own_drive_id: form.value.own_drive_id.trim(),
    }
  }
  if (form.value.driver === 'aliyun_drive_115') {
    return {
      refresh_token: form.value.refresh_token.trim(),
      qr_channel: form.value.qr_channel.trim(),
      root_folder_id: form.value.root_folder_id.trim() || 'root',
      drive_type: form.value.drive_type.trim() || 'resource',
      cookie: form.value.cookie.trim(),
      qr_source: form.value.qr_source.trim() || undefined,
    }
  }
  if (form.value.driver === 'aliyun_share_115') {
    return {
      refresh_token: form.value.refresh_token.trim(),
      open_copy_dir: form.value.open_copy_dir.trim() || 'root',
      share_url: form.value.share_url.trim(),
      share_code: form.value.share_code.trim(),
      share_pwd: form.value.passcode.trim(),
      cookie: form.value.cookie.trim(),
      qr_source: form.value.qr_source.trim() || undefined,
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
  if (
    (form.value.driver === 'aliyun_drive' || form.value.driver === 'aliyun_drive_115') &&
    !form.value.qr_channel.trim()
  ) {
    message.warning('请选择扫码通道（messense / xiaoya）')
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
          <NFormItem label="refresh_token" label-style="width: 100%">
            <template #label>
              <div class="storage-cookie-label">
                <span>refresh_token</span>
                <NButton size="small" type="primary" @click="openSync('baidu')">同步获取</NButton>
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
          <NFormItem label="Cookie" label-style="width: 100%">
            <template #label>
              <div class="storage-cookie-label">
                <span>Cookie</span>
                <NButton size="small" type="primary" @click="openSync('quark')">同步获取</NButton>
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
          <NFormItem label="Cookie" label-style="width: 100%">
            <template #label>
              <div class="storage-cookie-label">
                <span>Cookie</span>
                <NButton size="small" type="primary" @click="openSync('quark')">同步获取</NButton>
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
        </template>

        <template v-else-if="isM115">
          <NFormItem label="扫码设备（相同设备仅最新 Cookie 生效）">
            <NSelect
              v-model:value="form.qr_source"
              :options="m115QrSourceOptions"
              clearable
              size="small"
              placeholder="未选择（默认 115生活·支付宝小程序）"
            />
          </NFormItem>
          <NFormItem label="Cookie" label-style="width: 100%">
            <template #label>
              <div class="storage-cookie-label">
                <span>Cookie</span>
                <NButton size="small" type="primary" @click="openSync('m115')">同步获取</NButton>
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
          <NFormItem label="扫码设备（相同设备仅最新 Cookie 生效）">
            <NSelect
              v-model:value="form.qr_source"
              :options="m115QrSourceOptions"
              clearable
              size="small"
              placeholder="未选择（默认 115生活·支付宝小程序）"
            />
          </NFormItem>
          <NFormItem label="Cookie" label-style="width: 100%">
            <template #label>
              <div class="storage-cookie-label">
                <span>Cookie</span>
                <NButton size="small" type="primary" @click="openSync('m115')">同步获取</NButton>
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
        </template>

        <template v-else-if="isAliyunDrive || isAliyunDrive115">
          <NFormItem label="扫码通道（扫码通道和refresh_token必须对应）">
            <NSelect
              v-model:value="form.qr_channel"
              :options="[
                { label: 'messense.me（默认）', value: 'messense' },
                { label: 'xiaoya.pro', value: 'xiaoya' },
              ]"
              clearable
              placeholder="未设置（兼容旧配置）"
            />
          </NFormItem>
          <NFormItem label="阿里云盘 refresh_token" label-style="width: 100%">
            <template #label>
              <div class="storage-cookie-label">
                <span>阿里云盘 refresh_token</span>
                <NButton size="small" type="primary" @click="openSync('aliyunOpen')">同步获取</NButton>
                <NButton size="small" type="primary" @click="openAliyunQr">扫码获取</NButton>
              </div>
            </template>
            <NInput
              v-model:value="form.refresh_token"
              type="textarea"
              :autosize="{ minRows: 2, maxRows: 4 }"
              placeholder="开发平台 refresh_token，长度335位"
            />
          </NFormItem>
          <NFormItem label="云盘类型（drive_type）">
            <NSelect
              v-model:value="form.drive_type"
              :options="[
                { label: '资源库（resource，默认）', value: 'resource' },
                { label: '默认（default）', value: 'default' },
                { label: '备份盘（backup）', value: 'backup' },
              ]"
            />
          </NFormItem>
          <NFormItem v-if="isAliyunDrive" label="root_folder_id（挂载根目录）">
            <NInput v-model:value="form.root_folder_id" placeholder="root = 网盘根目录，也可填具体文件夹 file_id" />
          </NFormItem>
          <template v-if="isAliyunDrive115">
            <NFormItem label="扫码设备（相同设备仅最新 Cookie 生效）">
              <NSelect
                v-model:value="form.qr_source"
                :options="m115QrSourceOptions"
                clearable
                size="small"
                placeholder="未选择（默认 115生活·支付宝小程序）"
              />
            </NFormItem>
            <NFormItem label="115 Cookie" label-style="width: 100%">
              <template #label>
                <div class="storage-cookie-label">
                  <span>115 Cookie</span>
                  <NButton size="small" type="primary" @click="openSync('m115')">同步获取</NButton>
                  <NButton size="small" type="primary" @click="openQrcode('m115')">扫码获取</NButton>
                </div>
              </template>
              <NInput
                v-model:value="form.cookie"
                type="textarea"
                :autosize="{ minRows: 3, maxRows: 6 }"
                placeholder="登录 115.com 后，按 F12 → Application → Cookies 复制（需含 UID/CID/SEID）"
              />
            </NFormItem>
            <p class="storage-quark-tip">下载流程：SHA1 → 115 闪传（不耗云下载配额）→ 115 CDN 直链。</p>
          </template>
        </template>

        <template v-else-if="isAliyunShare || isAliyunShare115">
          <NFormItem label="分享地址">
            <NInput v-model:value="form.share_url" placeholder="https://www.alipan.com/s/xxxx（可带 ?pwd= 提取码）" />
          </NFormItem>
          <NFormItem label="提取码（非必填）">
            <NInput v-model:value="form.passcode" placeholder="分享链接未带 pwd 时使用此提取码" />
          </NFormItem>
          <NFormItem label="阿里云盘 refresh_token" label-style="width: 100%">
            <template #label>
              <div class="storage-cookie-label">
                <span>阿里云盘 refresh_token</span>
                <NButton size="small" type="primary" @click="openSync('aliyunShare')">同步获取</NButton>
                <NButton size="small" type="primary" @click="openAliyunWebQr">扫码获取</NButton>
              </div>
            </template>
            <NInput
              v-model:value="form.refresh_token"
              type="textarea"
              :autosize="{ minRows: 2, maxRows: 4 }"
              placeholder="网页版 refresh_token，长度32位"
            />
          </NFormItem>
          <p class="storage-quark-tip">
            <strong>注意：</strong>分享文件无 SHA1，下载前会先 <code>v2/file/copy</code> 转存到你自己的阿里云盘 （资源盘
            /xiaoya，不存在会自动创建），再从自己的盘取直链下载。
          </p>
          <NFormItem v-if="isAliyunShare" label="自己的网盘 drive_id（可选）">
            <NInput
              v-model:value="form.own_drive_id"
              placeholder="留空自动取资源库盘 resource_drive_id（推荐）；特殊需要时填其他盘 id"
            />
          </NFormItem>
          <template v-if="isAliyunShare115">
            <NFormItem label="扫码设备（相同设备仅最新 Cookie 生效）">
              <NSelect
                v-model:value="form.qr_source"
                :options="m115QrSourceOptions"
                clearable
                size="small"
                placeholder="未选择（默认 115生活·支付宝小程序）"
              />
            </NFormItem>
            <NFormItem label="115 Cookie" label-style="width: 100%">
              <template #label>
                <div class="storage-cookie-label">
                  <span>115 Cookie</span>
                  <NButton size="small" type="primary" @click="openSync('m115')">同步获取</NButton>
                  <NButton size="small" type="primary" @click="openQrcode('m115')">扫码获取</NButton>
                </div>
              </template>
              <NInput
                v-model:value="form.cookie"
                type="textarea"
                :autosize="{ minRows: 3, maxRows: 6 }"
                placeholder="登录 115.com 后，按 F12 → Application → Cookies 复制（需含 UID/CID/SEID）"
              />
            </NFormItem>
            <p class="storage-quark-tip">
              下载流程：分享文件 → 转存到你阿里云盘 → SHA1 → 115 闪传（不耗云下载配额）→ 115 CDN 直链。
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

    <!-- 同步获取：选择组内来源存储，选中即填入对应凭据字段 -->
    <NModal
      v-model:show="syncOpen"
      preset="card"
      title="选择账号来源"
      class="storage-dialog"
      :style="{ width: '420px' }"
      :bordered="false"
    >
      <NSelect
        v-model:value="syncSelected"
        :options="syncOptions"
        clearable
        placeholder="选择来源存储"
        @update:value="onSyncPick"
      />
    </NModal>

    <!-- 阿里云盘 open token 扫码（小雅双通道） -->
    <NModal
      v-model:show="aliyunQrDialog"
      preset="card"
      title="阿里云盘扫码获取 refresh_token"
      class="storage-dialog"
      :style="{ width: '360px' }"
      :bordered="false"
      @after-leave="stopAliyunQrPoll"
    >
      <p class="storage-quark-tip">
        请使用<strong>阿里云盘 App</strong> 扫码登录（手机端需已登录账号），扫码确认后自动回填 refresh_token
        与扫码通道。<br />「通道」决定 token 由哪个托管方签发，后续刷新将走同一通道，可在表单里随时修改。
      </p>
      <div class="qr-box">
        <img
          v-if="aliyunQrSvg"
          :src="'data:image/svg+xml;utf8,' + encodeURIComponent(aliyunQrSvg)"
          alt="阿里云盘登录二维码"
        />
        <NSpin v-else :show="aliyunQrGenerating" class="qr-loading" />
      </div>
      <div v-if="aliyunQrError" class="qr-error">{{ aliyunQrError }}</div>
      <template #footer>
        <div class="storage-dialog-actions">
          <NButton secondary @click="aliyunQrDialog = false">关闭</NButton>
          <NButton secondary :loading="aliyunQrGenerating" @click="genAliyunQrcode">刷新二维码</NButton>
        </div>
      </template>
    </NModal>

    <!-- 阿里云盘网页 token 扫码（alistgo 官方） -->
    <NModal
      v-model:show="aliyunWebQrDialog"
      preset="card"
      title="阿里云盘扫码获取 refresh_token"
      class="storage-dialog"
      :style="{ width: '360px' }"
      :bordered="false"
      @after-leave="stopAliyunWebQrPoll"
    >
      <p class="storage-quark-tip">
        请使用<strong>阿里云盘 App</strong> 扫码登录（手机端需已登录账号），扫码确认后自动回填网页版 refresh_token。<br />此驱动使用<strong
          >网页版 token</strong
        >（与「阿里云盘」驱动的 open token 不同）。
      </p>
      <div class="qr-box">
        <img
          v-if="aliyunWebQrSvg"
          :src="'data:image/svg+xml;utf8,' + encodeURIComponent(aliyunWebQrSvg)"
          alt="阿里云盘登录二维码"
        />
        <NSpin v-else :show="aliyunWebQrGenerating" class="qr-loading" />
      </div>
      <div v-if="aliyunWebQrError" class="qr-error">{{ aliyunWebQrError }}</div>
      <template #footer>
        <div class="storage-dialog-actions">
          <NButton secondary @click="aliyunWebQrDialog = false">关闭</NButton>
          <NButton secondary :loading="aliyunWebQrGenerating" @click="genAliyunWebQrcode">刷新二维码</NButton>
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
  gap: 8px;
  width: 100%;
  flex: 1;
}
/* 三个子项会被 space-between 分散（按钮悬中间）；改用文字 margin-right:auto
   吃掉剩余空间，两个按钮一起贴右紧挨 */
.storage-cookie-label > span:first-child {
  margin-right: auto;
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
.qr-channel-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin: 10px 0 2px;
}
.qr-channel-label {
  font-size: 12px;
  color: var(--m3-on-surface-variant, #5f6368);
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
  dialog's inner DOM. These global rules are matched by :has on the
  cookie-label row. naive wraps the label slot in an inline
  .n-form-item-label__text span; display:contents removes that box so the
  .storage-cookie-label flex row becomes a direct child of the full-width
  label and its space-between pushes the buttons flush right.
-->
<style>
.n-form-item-label:has(.storage-cookie-label) {
  display: flex;
  align-items: center;
  width: 100% !important;
}
.n-form-item-label__text:has(.storage-cookie-label) {
  display: contents !important;
}
</style>
