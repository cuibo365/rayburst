<script setup lang="ts">
/**
 * Storage manager (存储): alist-style storage CRUD over drivers.
 * Adding a Baidu Netdisk storage fills client_id/client_secret with defaults;
 * the refresh_token field carries an "获取" button that opens the OAuth page.
 */
import { computed, onMounted, ref } from 'vue'
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

interface BaiduAddition {
  refresh_token: string
  access_token?: string
  token_expires_at?: number
  root_path: string
  client_id: string
  client_secret: string
}

const DEFAULT_CLIENT_ID = 'hq9yQ9w9kR4YHj1kyYafLygVocobh7Sf'
const DEFAULT_CLIENT_SECRET = 'YH2VpZcFJHYNnV6vLfHQXDBhcE7ZChyE'
const CALLBACK_URL = 'https://alistgo.com/tool/baidu/callback'

const DRIVERS = [{ label: '百度网盘', value: 'baidu_netdisk' }] as const

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
  remark: string
}>({
  mount_path: '/百度网盘',
  driver: 'baidu_netdisk',
  refresh_token: '',
  client_id: DEFAULT_CLIENT_ID,
  client_secret: DEFAULT_CLIENT_SECRET,
  root_path: '/',
  remark: '',
})

const dialogTitle = computed(() => (editingId.value === null ? '添加存储' : '编辑存储'))
const isBaidu = computed(() => form.value.driver === 'baidu_netdisk')

function resetForm() {
  editingId.value = null
  form.value = {
    mount_path: '/百度网盘',
    driver: 'baidu_netdisk',
    refresh_token: '',
    client_id: DEFAULT_CLIENT_ID,
    client_secret: DEFAULT_CLIENT_SECRET,
    root_path: '/',
    remark: '',
  }
}

function openAdd() {
  resetForm()
  showDialog.value = true
}

function openEdit(s: CloudStorage) {
  editingId.value = s.id
  let addition: Partial<BaiduAddition> = {}
  try {
    addition = JSON.parse(s.addition) as Partial<BaiduAddition>
  } catch {
    addition = {}
  }
  form.value = {
    mount_path: s.mount_path,
    driver: s.driver,
    refresh_token: addition.refresh_token ?? '',
    client_id: addition.client_id ?? DEFAULT_CLIENT_ID,
    client_secret: addition.client_secret ?? DEFAULT_CLIENT_SECRET,
    root_path: addition.root_path ?? '/',
    remark: s.remark,
  }
  showDialog.value = true
}

function onDriverChange() {
  // Re-apply Baidu defaults whenever the driver switches (or is first picked).
  form.value.client_id = DEFAULT_CLIENT_ID
  form.value.client_secret = DEFAULT_CLIENT_SECRET
}

async function openAuthorize() {
  const clientId = form.value.client_id.trim() || DEFAULT_CLIENT_ID
  const url = `https://openapi.baidu.com/oauth/2.0/authorize?response_type=code&client_id=${encodeURIComponent(clientId)}&redirect_uri=${encodeURIComponent(CALLBACK_URL)}&scope=basic,netdisk&qrcode=1`
  try {
    await openUrl(url)
  } catch (e) {
    message.error(`打开授权页面失败：${String(e)}`)
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
  saving.value = true
  try {
    const addition: BaiduAddition = {
      refresh_token: form.value.refresh_token.trim(),
      root_path: form.value.root_path.trim() || '/',
      client_id: form.value.client_id.trim() || DEFAULT_CLIENT_ID,
      client_secret: form.value.client_secret.trim() || DEFAULT_CLIENT_SECRET,
    }
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
    message.error(((e as Record<string, unknown>).message as string) ?? String(e))
  } finally {
    saving.value = false
  }
}

async function toggleDisabled(s: CloudStorage) {
  try {
    await invoke('cloud_storage_toggle', { id: s.id, disabled: !s.disabled })
    await load()
  } catch (e) {
    message.error(((e as Record<string, unknown>).message as string) ?? String(e))
  }
}

async function remove(s: CloudStorage) {
  try {
    await invoke('cloud_storage_delete', { id: s.id })
    message.success('存储已删除')
    await load()
  } catch (e) {
    message.error(((e as Record<string, unknown>).message as string) ?? String(e))
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
    message.error(((e as Record<string, unknown>).message as string) ?? String(e))
  } finally {
    loading.value = false
  }
}

onMounted(load)
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
                {{ s.driver === 'baidu_netdisk' ? '百度网盘' : s.driver }}
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
          <NInput v-model:value="form.mount_path" placeholder="/百度网盘" />
        </NFormItem>
        <NFormItem label="驱动">
          <NSelect v-model:value="form.driver" :options="[...DRIVERS]" @update:value="onDriverChange" />
        </NFormItem>

        <template v-if="isBaidu">
          <NFormItem label="refresh_token">
            <div class="storage-token-row">
              <NInput
                v-model:value="form.refresh_token"
                type="textarea"
                :autosize="{ minRows: 2, maxRows: 4 }"
                placeholder="粘贴 refresh_token（以 122. 开头）"
              />
              <NButton size="small" secondary class="storage-token-get" @click="openAuthorize"> 获取 </NButton>
            </div>
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
.storage-token-row {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  width: 100%;
}
.storage-token-get {
  flex: none;
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
