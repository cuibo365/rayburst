<script setup lang="ts">
/**
 * Cloud drive (网盘) entry page.
 *
 * - Baidu Netdisk (应用内): self-hosted UI rendered by BaiduDriveApp.vue, the
 *   Rust backend talks to Baidu's APIs (QR login -> list -> download).
 * - Baidu Netdisk (页面内): full-screen navigation mode on the main webview
 *   (injected back/download buttons).
 * - Quark / 115 open their web versions in the default browser.
 */
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { NIcon } from 'naive-ui'
import { CloudOutline, AlbumsOutline, FolderOpenOutline, ChevronBackOutline, ChevronBack } from '@vicons/ionicons5'
import { invoke } from '@tauri-apps/api/core'
import { openUrl } from '@tauri-apps/plugin-opener'
import { useAppMessage } from '@/composables/useAppMessage'
import BaiduDriveApp from './baidu/BaiduDriveApp.vue'

const { t } = useI18n()
const message = useAppMessage()

const PAN_MAIN_URL = 'https://pan.baidu.com/disk/main'
const mode = ref<'cards' | 'baidu-app'>('cards')

async function openBaiduWeb() {
  // Full-screen navigation mode: reuse the MAIN window's webview by navigating
  // the whole app page to Baidu Netdisk. Rust drives the navigation and keeps
  // injecting the bridge script (download + back button).
  try {
    await invoke('cloud_drive_baidu_nav_open', { url: PAN_MAIN_URL })
  } catch (err) {
    console.error('cloud-drive: nav open failed', err)
    const msg = err instanceof Error ? err.message : String(err)
    message.error(msg || t('cloudDrive.openFailed'))
  }
}

async function openQuark() {
  await openUrl('https://pan.quark.cn').catch((e) => console.error('cloud-drive: open quark failed', e))
}

async function open115() {
  await openUrl('https://115.com/').catch((e) => console.error('cloud-drive: open 115 failed', e))
}
</script>

<template>
  <div class="cloud-drive-page">
    <template v-if="mode === 'cards'">
      <header class="cloud-drive-header">
        <h2>{{ t('navigation.cloud-drive') }}</h2>
        <p class="cloud-drive-hint">{{ t('cloudDrive.pageHint') }}</p>
      </header>

      <div class="cloud-drive-cards">
        <button type="button" class="drive-card drive-card--baidu-app" @click="mode = 'baidu-app'">
          <NIcon :size="34" aria-hidden="true"><CloudOutline /></NIcon>
          <span class="drive-card__name">{{ t('cloudDrive.baiduApp') }}</span>
          <span class="drive-card__desc">{{ t('cloudDrive.baiduAppDesc') }}</span>
          <span class="drive-card__action">
            <NIcon :size="15" aria-hidden="true"><ChevronBackOutline /></NIcon>
            {{ t('cloudDrive.openApp') }}
          </span>
        </button>

        <button type="button" class="drive-card drive-card--baidu" @click="openBaiduWeb">
          <NIcon :size="34" aria-hidden="true"><CloudOutline /></NIcon>
          <span class="drive-card__name">{{ t('cloudDrive.baiduNav') }}</span>
          <span class="drive-card__desc">{{ t('cloudDrive.baiduNavDesc') }}</span>
          <span class="drive-card__action">
            <NIcon :size="15" aria-hidden="true"><ChevronBackOutline /></NIcon>
            {{ t('cloudDrive.openInPage') }}
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

    <template v-else>
      <div class="baidu-app-bar">
        <button type="button" class="baidu-app-back" @click="mode = 'cards'">
          <NIcon :size="15" aria-hidden="true"><ChevronBack /></NIcon>
          {{ t('cloudDrive.backToCards') }}
        </button>
      </div>
      <BaiduDriveApp />
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
.drive-card--baidu-app {
  background: linear-gradient(160deg, rgba(59, 130, 246, 0.14), rgba(59, 130, 246, 0.03));
}

.baidu-app-bar {
  margin-bottom: 12px;
}
.baidu-app-back {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  border: none;
  background: transparent;
  color: var(--m3-on-surface-variant);
  font-size: 13px;
  cursor: pointer;
  padding: 4px 8px;
  border-radius: 6px;
}
.baidu-app-back:hover {
  background: rgba(128, 128, 128, 0.15);
  color: var(--m3-on-surface);
}
</style>
