import { createRouter, createWebHashHistory } from 'vue-router'
import Home from './views/Home.vue'

const router = createRouter({
  history: createWebHashHistory(),
  scrollBehavior: () => ({ top: 0 }),
  routes: [
    { path: '/', name: 'Home', component: Home },
    {
      path: '/library',
      name: 'Library',
      component: () => import('./features/v2/views/V2Recipes.vue')
    },
    {
      path: '/library/:id',
      name: 'V2Runner',
      component: () => import('./features/v2/views/V2Runner.vue')
    },
    {
      path: '/tasks',
      name: 'Tasks',
      component: () => import('./features/v2/views/V2Tasks.vue')
    },
    {
      path: '/connections',
      name: 'Connections',
      component: () => import('./features/connections/views/Connections.vue')
    },
    {
      path: '/settings',
      name: 'Settings',
      component: () => import('./features/settings/views/Settings.vue')
    },
    // 未匹配路径兜底：回到首页而不是空白。
    { path: '/:pathMatch(.*)*', redirect: '/' }
  ]
})

export default router
