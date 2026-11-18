# 17 · 秘林咖啡馆 — 兽人班次(Hyper3D 资产经营小游戏)

> 引擎原生渲染的经营小游戏:裸 WebGL2 + Cook-Torrance PBR(与
> `crates/oriera-render::SHADER_VIEW_PBR` 公式同源,MR 贴图同口径),
> 角色 = Hyper3D Rodin 自有资产包(500k 面 QC 全过,静态网格)。
> 零依赖、离线可玩;无 GLB 时自动降级为程序化占位兽,游戏循环不受影响。

[▶ 本地运行](http://127.0.0.1:8123/examples/17-myth-cafe/):`python -m http.server` 后打开本页。

## 玩法

- 你是石魔老板「磐叔」,深夜秘林咖啡馆的怪兽常客会走到树桩座位点单
- 看准客人头顶的饮品气泡(莓果昔 🧋 / 松露拿铁 ☕ / 萤光茶 🍵),
  点下方对应出品键出餐:对单 +金币(剩余耐心越多小费越高),送错不扣分但浪费时间
- 客人等急了会气跑扣一颗心;三心用完打烊结算
- 难度随满意客数递增:出客更密、耐心更短
- 拖动旋转视角 · 滚轮缩放

## 资产(许可与来源)

- 角色全部来自 **Hyper3D Rodin 自有资产包**(AI 生成,自有/自用渠道):
  `barista_golem`=creature_stone_golem(全量 500k 面)、
  顾客 cust_phoenix/cust_treant/cust_griffon/cust_wolf
  = creature_ember_phoenix / mushroom_treant / griffon / ice_wolf
  (60k 面聚类简化,贴图 1K,保 PBR 质感;简化工具见 `out/skinning-m1/_decimate_glb.py`)
- 资产为 AI 生成件,按自有渠道使用;**不冒充人工建模交付给拒收 AI 数据的买方**

## 资产文件与本仓库

`assets/*.glb` 体积较大(27MB 全模 + 4×10-16MB),**不进 git**
(仓库克隆友好);本地把资产包解压后按上述命名拷入 `assets/` 即可。
缺文件时游戏自动用程序化占位兽营业(功能完整,观感降级)。

## 工程注记(诚实降级项)

- 角色为静态网格(资产包无骨骼):待机=呼吸浮动+摇头,行走=位移+小跳,
  满意=双跳,生气=发抖——全部程序化;骨骼蒙皮管线本身由
  `crates/oriera-render`(GPU `render_skinned_views` + CesiumMan 互证)与
  `engine/skin.js`(双端金样 `contracts/golden/skin_golden.json`)承载,
  资产包未来出骨骼件即可直接接入
- 夜景为半球环境+单方向光近似,无阴影贴图;角色脚下以贴地影盘示意
- 软件渲染(无 GPU 的环境)下 150 万面会明显掉帧;真机 GPU 全速
