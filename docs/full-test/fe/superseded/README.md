# 已归档：子流程「全屏浮层」设计器测试（superseded）

这两个测试（`subflow_designer.cjs` 冒烟 + `subflow_designer_deep.cjs` 深度）验证的是**上一轮的全屏浮层**
子流程编辑器（`.flow-sub-mask` / `[data-sub-close]` / `[data-sub-act]` / `[data-sub-prop]` …）。

该浮层在真实门户（explorer/content/property 三个独立 region 宿主、property 窄区祖先造 containing block）里
被裁剪、关闭按钮够不到 → 用户报「覆盖整个页面且无法返回，直接失控」。已改为**四区钻入式**（复用 content
单一画布 + 常驻工具栏「← 返回主流程」+ explorer 变体列表 + property「数据模型」页签），浮层全套已删除。

**取代者**：`../subflow_drilldown.cjs`（门户级多区真机测试，23/23）——同时覆盖布局无失控 + 导航 +
功能持久化（save→subSave、publish 版本+1、新变体唯一 key/自动绑定/isSubflow/重存不重复、数据模型页签）。
这两个文件保留仅作历史留档，**不再执行**（其断言的 DOM 已不存在）。
