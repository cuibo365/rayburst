/** @fileoverview Vue Router configuration with task and preference routes. */
import { createRouter, createWebHistory } from 'vue-router'

const router = createRouter({
  history: createWebHistory(),
  routes: [
    {
      path: '/',
      component: () => import('@/layouts/MainLayout.vue'),
      children: [
        {
          path: '',
          redirect: '/task/all',
        },
        {
          path: '/task/:status(all|progress|failed|completed)?',
          name: 'task',
          component: () => import('@/views/TaskView.vue'),
          props: true,
        },
        {
          path: '/cloud-drive',
          name: 'cloud-drive',
          component: () => import('@/views/CloudDriveView.vue'),
          children: [
            {
              // `alias: ''` (rather than a separate `path: ''` + redirect)
              // lets the sidebar link straight to this leaf route, so entering
              // the drive is a single navigation. An extra redirect route made
              // it a two-step navigation whose empty intermediate component
              // deadlocked the `Transition mode="out-in"` wrapper in
              // CloudDriveView, leaving the first render blank.
              path: 'home',
              alias: '',
              name: 'cloud-drive-home',
              component: () => import('@/views/DriveBrowserView.vue'),
            },
            {
              path: 'storage',
              name: 'cloud-drive-storage',
              component: () => import('@/views/StorageManagerView.vue'),
            },
          ],
        },
        {
          path: '/preference',
          name: 'preference',
          component: () => import('@/views/PreferenceView.vue'),
          children: [
            {
              path: 'general',
              alias: '',
              name: 'preference-general',
              component: () => import('@/components/preference/General.vue'),
            },
            {
              path: 'downloads',
              name: 'preference-downloads',
              component: () => import('@/components/preference/Downloads.vue'),
            },
            {
              path: 'bt',
              name: 'preference-bt',
              component: () => import('@/components/preference/BitTorrent.vue'),
            },
            {
              path: 'ed2k',
              name: 'preference-ed2k',
              component: () => import('@/components/preference/Ed2k.vue'),
            },
            {
              path: 'network',
              name: 'preference-network',
              component: () => import('@/components/preference/Network.vue'),
            },
            {
              path: 'advanced',
              name: 'preference-advanced',
              component: () => import('@/components/preference/Advanced.vue'),
            },
          ],
        },
      ],
    },
    {
      path: '/:pathMatch(.*)*',
      redirect: '/',
    },
  ],
})

export default router
