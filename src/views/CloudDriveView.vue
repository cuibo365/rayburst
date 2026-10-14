<script setup lang="ts">
/**
 * Cloud drive (网盘) entry page.
 *
 * - Baidu Netdisk opens inside the app as an embedded child webview that
 *   fills the content area (see Rust `cloud_drive_baidu_open`).
 * - Quark / 115 open their web versions in the default browser.
 */
import { onMounted, onUnmounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { NIcon } from 'naive-ui'
import { CloudOutline, AlbumsOutline, FolderOpenOutline, ChevronBackOutline } from '@vicons/ionicons5'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { openUrl } from '@tauri-apps/plugin-opener'

interface WebviewBounds {
  x: number
  y: number
  width: number
  height: number
}

const { t } = useI18n()
const rootRef = ref<HTMLElement | null>(null)
const viewing = ref(false)
const opening = ref(false)
let unlistenHidden: UnlistenFn | null = null

function measure(): WebviewBounds | null {
  const el = rootRef.value
  if (!el) return null
  const rect = el.getBoundingClientRect()
  return { x: rect.x, y: rect.y, width: rect.width, height: rect.height }
}

async function openBaidu() {
  if (opening.value) return
  const bounds = measure()
  if (!bounds) return
  opening.value = true
  try {
    await invoke('cloud_drive_baidu_open', { bounds })
    viewing.value = true
  } catch (err) {
    console.error('cloud-drive: open baidu failed', err)
  } finally {
    opening.value = false
  }
}

async function closeBaidu() {
  viewing.value = false
  try {
    await invoke('cloud_drive_baidu_close')
  } catch (err) {
    console.error('cloud-drive: close baidu failed', err)
  }
}

async function openQuark() {
  await openUrl('https://pan.quark.cn').catch((e) => console.error('cloud-drive: open quark failed', e))
}

async function open115() {
  await openUrl('https://115.com/').catch((e) => console.error('cloud-drive: open 115 failed', e))
}

function onResize() {
  if (!viewing.value) return
  const bounds = measure()
  if (bounds) {
    invoke('cloud_drive_baidu_resize', { bounds }).catch((e) => console.error('cloud-drive: resize failed', e))
  }
}

onMounted(async () => {
  window.addEventListener('resize', onResize)
  unlistenHidden = await listen('cloud-drive:hidden', () => {
    viewing.value = false
  })
})

onUnmounted(() => {
  window.removeEventListener('resize', onResize)
  unlistenHidden?.()
  if (viewing.value) {
    void closeBaidu()
  }
})
</script>

<template>
  <div ref="rootRef" class="cloud-drive-page" :data-viewing="viewing">
    <template v-if="!viewing">
      <header class="cloud-drive-header">
        <h2>{{ t('navigation.cloud-drive') }}</h2>
        <p class="cloud-drive-hint">{{ t('cloudDrive.pageHint') }}</p>
      </header>

      <div class="cloud-drive-cards">
        <button type="button" class="drive-card drive-card--baidu" :disabled="opening" @click="openBaidu">
          <NIcon :size="34" aria-hidden="true"><CloudOutline /></NIcon>
          <span class="drive-card__name">{{ t('cloudDrive.baidu') }}</span>
          <span class="drive-card__desc">{{ t('cloudDrive.baiduDesc') }}</span>
          <span class="drive-card__action">
            <NIcon :size="15" aria-hidden="true"><ChevronBackOutline /></NIcon>
            {{ t('cloudDrive.openInApp') }}
          </span>
        </button>

        <button type="button" class="drive-card drive-card--quark" @click="openQuark">
          <NIcon :size="34" aria-hidden="true"><AlbumsOutline /></NIcon>
          <span class="drive-card__name">{{ t('cloudDrive.quark') }}</span>
          <span class="drive-card__desc">{{ t('cloudDrive.quarkDesc') }}</span>
          <span class="drive-card__action">
            <NIcon :size="15" aria-hidden="true"><ChevronBackOutline /></NIcon>
            {{ t('cloudDrive.openWeb') }}
          </span>
        </button>

        <button type="button" class="drive-card drive-card--115" @click="open115">
          <NIcon :size="34" aria-hidden="true"><FolderOpenOutline /></NIcon>
          <span class="drive-card__name">{{ t('cloudDrive.netdisk115') }}</span>
          <span class="drive-card__desc">{{ t('cloudDrive.netdisk115Desc') }}</span>
          <span class="drive-card__action">
            <NIcon :size="15" aria-hidden="true"><ChevronBackOutline /></NIcon>
            {{ t('cloudDrive.openWeb') }}
          </span>
        </button>
      </div>
    </template>
  </div>
</template>

<style scoped>
.cloud-drive-page {
  height: 100%;
  min-height: 0;
  overflow: auto;
  padding: 24px 28px;
  box-sizing: border-box;
}
.cloud-drive-header {
  margin-bottom: 20px;
}
.cloud-drive-header h2 {
  margin: 0 0 6px;
  font-size: 20px;
  font-weight: 600;
  color: var(--m3-on-surface);
}
.cloud-drive-hint {
  margin: 0;
  font-size: 13px;
  color: var(--m3-on-surface-variant);
}
.cloud-drive-cards {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
  gap: 16px;
  max-width: 900px;
}
.drive-card {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 10px;
  padding: 20px;
  border: 1px solid var(--m3-outline-variant, rgba(128, 128, 128, 0.35));
  border-radius: var(--radius-lg, 12px);
  background: var(--m3-surface);
  color: var(--m3-on-surface);
  text-align: left;
  cursor: pointer;
  transition:
    border-color 180ms ease,
    box-shadow 180ms ease,
    transform 180ms ease;
}
.drive-card:hover {
  border-color: var(--m3-primary);
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.12);
  transform: translateY(-1px);
}
.drive-card:focus-visible {
  outline: 2px solid var(--m3-primary);
  outline-offset: -2px;
}
.drive-card:disabled {
  opacity: 0.6;
  cursor: default;
}
.drive-card__name {
  font-size: 16px;
  font-weight: 600;
}
.drive-card__desc {
  font-size: 12.5px;
  line-height: 1.5;
  color: var(--m3-on-surface-variant);
}
.drive-card__action {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  margin-top: 6px;
  font-size: 13px;
  color: var(--m3-primary);
}
.drive-card__action :deep(.n-icon) {
  transform: rotate(180deg);
}
</style>
