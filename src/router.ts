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
      // 「结果」：产物自己的入口。产品愿景把"结果"和连接/应用/任务并列为四个产品名词，
      // 而在此之前产物**只能顺着任务找到**（要先想起是哪次跑的）。
      path: '/results',
      name: 'Results',
      component: () => import('./features/v2/views/V2Results.vue')
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
