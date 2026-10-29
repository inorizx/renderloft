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
      // 「结果」页已删（2026-10-29）：它前提是"产物会积累成资产库"，而产物本来就是
      // **跟着任务走的交付物**（`http-api.md`：「不长期托管你的字节」，保留期到点即清），
      // 任务历史页也早已能看图、能批量取、能连产物一起删。
      // 保留一条重定向只为旧书签：不落到首页，而是落到真正管这些东西的地方。
      path: '/results',
      redirect: '/tasks'
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
