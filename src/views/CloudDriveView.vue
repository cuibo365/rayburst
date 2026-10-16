<script setup lang="ts">
/**
 * Cloud drive (网盘) shell: route-backed tabs (首页 / 存储), mirroring the
 * preference view pattern. The browse sub-page is treated as a child of 首页
 * so the tab stays highlighted while browsing a storage.
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'
import { NTabs, NTab } from 'naive-ui'
import { cloudDriveDestinations } from '@/components/layout/navigation'

const { t } = useI18n()
const route = useRoute()
const router = useRouter()

const selectedTab = computed(() => {
  const name = String(route.name ?? '')
  if (name === 'cloud-drive-home') return 'cloud-drive-home'
  if (name === 'cloud-drive-storage') return 'cloud-drive-storage'
  return 'cloud-drive-home'
})

function navigate(name: string) {
  void router.push({ name })
}
</script>

<template>
  <div class="cloud-drive-view">
    <NTabs class="cloud-drive-tabs" :value="selectedTab" type="line" @update:value="navigate">
      <NTab v-for="key in cloudDriveDestinations" :key="key" :name="'cloud-drive-' + key">
        {{ t('cloudDrive.' + key) }}
      </NTab>
    </NTabs>
    <div class="panel-body">
      <router-view v-slot="{ Component, route: innerRoute }">
        <Transition name="fade" mode="out-in">
          <component :is="Component" :key="innerRoute.path" />
        </Transition>
      </router-view>
    </div>
  </div>
</template>

<style scoped>
.cloud-drive-view {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
}
.cloud-drive-tabs {
  flex: none;
  padding: 0 24px;
}
.panel-body {
  flex: 1;
  min-height: 0;
  padding: 16px 24px 24px;
  overflow: hidden;
}
</style>
