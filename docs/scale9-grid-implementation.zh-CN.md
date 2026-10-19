# 九宫格渲染实现与排查记录

记录日期：2026-10-16。基于 Ruffle `89a704996`（Release 0.7.1），工作与交付分支为本地
`0.7.1-A`，推送到 `aojiaoxiaolinlin/ruffle` fork。

本文记录当前实现和验证范围，供后续复现、优化和升级上游代码时参考。
这是一版核心层矢量九宫格渲染实现，尚不代表全部 Flash 行为已经兼容。

## 1. 问题来源与定位证据

报告现象：游戏说明面板背景随尺寸变化而拉伸，边框和圆角变形。

样本地址：<http://enter.wanwan4399.com/bin-debug/GreenGame.swf>。
分析工具：JPEXS FFDec 26.3.0，下载地址：
<https://github.com/jindrapetrik/jpexs-decompiler/releases/download/version26.3.0/ffdec_26.3.0.zip>。

已核实的数据：

| 项目 | 样本值 |
| --- | --- |
| character 75 | 单帧 DefineSprite |
| 内部图形 | character 74，DefineShape4 |
| 图形变换 | sprite 内使用恒等矩阵放置 shape 74 |
| 网格标签 | DefineScalingGrid，characterId = 75 |
| 网格范围，twips | x = 1155..3514，y = 2540..4659 |
| 网格矩形，像素 | Rectangle(57.75, 127, 117.95, 105.95) |
| 使用位置 | character 76 中的实例 `bgin`，depth = 126 |
| 该实例初始缩放 | scaleX ≈ 1.291153，scaleY ≈ 0.3582306 |
| 填充类型 | 包含纯色和两个线性渐变填充 |

换算关系为 20 twips = 1 像素。矩形最后两项是宽、高，不是右、下边界。

修改前，`MovieClip::define_scaling_grid` 可以读取网格，
`TDisplayObject::set_scaling_grid` 可以保存它，ActionScript API 也可以读写。
但是矢量渲染没有使用网格，最终仍把完整图形交给同一仿射变换缩放。
因此，标签存在并不意味着渲染功能已经实现。

本次未收到原报告截图，也未跑通完整游戏交互。已验证的是样本标签、
本地代码路径，以及从样本抽出的独立背景元件。

## 2. 实现入口与文件职责

以下路径相对于仓库根目录。

| 文件 | 修改内容 |
| --- | --- |
| `core/src/display_object/scale9_grid.rs` | 网格归属、轴向分段、切片裁剪、渐变补偿及缓存键 |
| `core/src/display_object/graphic.rs` | 静态 Shape 和动态 Shape 绘制接入；静态图形的渐变变体缓存 |
| `core/src/display_object/morph_shape.rs` | MorphShape 绘制接入；按插值帧保存渐变变体缓存 |
| `core/src/display_object/movie_clip.rs` | 仅对自身 Drawing 应用网格；读取 Sprite/Button 的网格标签 |
| `core/src/drawing.rs` | 提取可复用的 DistilledShape 构造；动态 Drawing 的渐变变体缓存 |
| `core/src/display_object.rs` | 网格修改时使位图缓存失效；缓存键检查；离屏绘制根对象状态保存和恢复 |
| `core/src/context.rs` | 增加 `RenderContext::untransformed_object` |
| `core/src/player.rs` | 初始化正常绘制上下文的新字段 |
| `core/src/bitmap/operations.rs` | 初始化 BitmapData.draw 上下文的新字段 |

实现位于核心层，复用现有 `CommandHandler` 的图形和 stencil 裁剪命令，
没有新增后端接口或 GPU shader。实际图像验证使用原生 WGPU/Vulkan；
WebGL、Canvas 及浏览器运行效果尚未单独验证。

## 3. 网格归属与绘制流程

`cache_key` 按以下顺序选择网格：

1. 对象自身有有效网格时使用自身网格。
2. Graphic / MorphShape 没有自身网格时，尝试使用直接父对象的网格。
3. 普通子 MovieClip、文字和独立 Bitmap 不通过本次矢量切片入口。
4. 如果对象或网格归属对象是本次忽略自身变换的绘制根，则不应用其网格。

MovieClip 仅将自身 Drawing 送入切片入口，再按原有路径渲染子对象。
这样不会把说明文字或整个子 MovieClip 跟背景一起切成九块。

渲染时，把包围盒按网格分成最多九个非空矩形。每个矩形依次：

1. 绘制裁剪矩形并激活 stencil mask。
2. 向变换栈压入该片的修正矩阵。
3. 绘制矢量图形。
4. 弹出变换，重绘裁剪矩形以清理 stencil，然后结束 mask。

现有变换栈已包含普通缩放，所以这里压入的是相对于普通缩放的修正量。
若 Shape 使用父对象网格，先转换到父坐标系，再做修正，最后转换回 Shape
坐标系。代码中的形式为：

```text
correction = from_owner * slice.transform * to_owner
```

不要在这个入口直接再次应用完整对象缩放，否则会出现重复缩放。

## 4. 单轴计算、回退和数值精度

设源轴范围为 `[min, max]`，网格中心范围为 `[start, end]`，
对象缩放绝对值为 `s`：

```text
leading  = start - min
trailing = max - end
center   = (max - min) * s - leading - trailing
```

两端区域保持原尺寸，中间区域吸收剩余尺寸。位置仍以对象注册点为基准，
不能把包围盒左上角误当成原点；源包围盒可能有负坐标。
负缩放保留在原变换中，轴向修正使用缩放的绝对值，因此可以镜像。

当前回退规则：

- 网格归属对象的矩阵 `b` 或 `c` 非零：整个网格回退到普通缩放。
- 无效包围盒、越出包围盒的网格、非正网格宽高：回退。
- 单轴缩放为零、非有限数值，或剩余中心小于 1 twip：该轴回退。
- 两轴均无需修正：走原绘制路径。
- 子对象到网格坐标的矩阵不可逆：回退。

缩小阈值及单轴独立回退是当前实现策略，尚未用真实 Flash Player 对所有
临界值逐像素验证。原样本 scaleY ≈ 0.358 时，其纵向固定区域可能放不下，
排查时应先计算 `center`，不能把所有缩小后的变形都视为切片没有生效。

矩阵系数使用浮点数，平移最终舍入为整数 twips。若出现细缝或一像素跳动，
需同时记录矩阵、缩放、裁剪边界、抗锯齿质量和视口缩放。

## 5. 渐变处理

仅分片重绘原始 Shape 会让渐变也随每片分别缩放。为保持渐变在整体普通
变换下的分布，`GradientCache` 为切片生成补偿后的填充样式：

```text
compensated_gradient_matrix = inverse(correction) * original_gradient_matrix
```

支持补偿 LinearGradient、RadialGradient、FocalGradient，以及描边中的
渐变填充。纯色不需要生成变体，Bitmap 填充没有使用这项渐变补偿。

缓存以修正矩阵和细分缩放值为键，最多保留 32 个变体，命中后移到队尾。
动态 Drawing 修改或清空时清理这些变体，避免复用旧路径或旧填充。
静态 Shape 的变体在共享图形数据中保存；MorphShape 的变体按插值帧保存。

线性、径向渐变已做自动对照测试；焦点渐变、渐变描边和复杂插值组合仍需
扩展测试，不能仅凭接入代码认定所有组合已经验证。

## 6. 位图缓存与 BitmapData.draw

网格会改变视觉结果，即使对象尺寸和世界矩阵没有变化。
因此 `set_scaling_grid` 在值变化时调用 `invalidate_cached_bitmap`。

`BitmapCache` 还保存网格缓存键，包含源包围盒、网格、网格归属对象的
线性矩阵部分，以及 Shape 到归属对象的变换。键变化会强制缓存重绘。
这也覆盖了“父网格变化，但缓存子 Shape 的自身矩阵和尺寸未变”的情况。
归属对象自身的平移不参与键，避免仅移动对象就重建切片缓存。

`BitmapData.draw` 忽略源对象自身的变换。若直接读取源对象存储的 scale
来切片，即使实际绘制没有使用该 scale，也会错误扭曲图形。
`RenderContext::untransformed_object` 用于记录此类绘制根；
`render_base` 保存、设置并在绘制结束后恢复它，离屏缓存上下文也继承该值。

回归测试同时覆盖直接绘制有网格的 Sprite，以及直接绘制一个带父网格的
Shape。后者必须忽略未参与此次绘制的父对象网格。

## 7. 测试与本次验证记录

从仓库根目录执行：

```sh
cargo test -p ruffle_core scale9_grid --lib --locked
cargo test -p tests --features imgtests --test tests scale9_grid --locked
cargo test -p tests --features imgtests --test tests cab_mask_transform --locked
cargo test -p tests --features imgtests --test tests graphics_draw_ --locked
cargo clippy -p ruffle_core --locked -- -D warnings
cargo fmt --all --check
git diff --check
```

依赖已缓存时可增加 `--offline`；首次运行可能需要下载依赖。
图像测试需要可用的 GPU 后端或软件 Vulkan 驱动。

已通过的验证：

- 3 个单元测试：注册点和边缘尺寸、缩小和镜像、退化轴。
- `avm2/scale9_grid`：九种布局，三次图像捕获，直接离屏绘制像素检查。
- `avm2/scale9_grid_gradients`：线性和径向渐变，各自覆盖 Sprite 和子 Shape。
- 现有 `visual/cache_as_bitmap/cab_mask_transform` 用例。
- 现有 `avm2/graphics_draw_path`、`avm2/graphics_draw_triangles` 用例。
- Clippy（警告视为错误）、格式与 diff 空白检查。

`scale9_grid` 测试运行 4 ticks，在第 1、3、4 tick 捕获图像。
构造后第一次 ENTER_FRAME 并非构造所在 tick，不能误把第二帧状态捕获提前。
测试会修改网格，再清除网格，以检查缓存刷新。

三张预期图由固定纯色矩形的解析坐标构造，**不是 Flash Player 截图**。
具体边界见测试目录 README。渐变测试比较同一矩形在有、无网格情况下的
填充颜色，每通道允许 2 的误差。后续应补充真实 Flash Player 对照。

本机运行时 GLES 探测曾输出 `eglInitialize` / DRI2 错误，但图像测试最终
使用 Vulkan 成功完成。判断结果应看实际后端和用例结果，不应仅凭探测日志
认定渲染失败。

独立游戏样本验证：取出 shape 74、sprite 75 和网格，另外复制一个不带
网格的 Sprite，在相同横向缩放下，对比纵向 0.3582306 与 1.5 两组结果。
九宫格版本的边框、圆角和渐变已目视检查。游戏 SWF、FFDec 下载和临时截图
没有加入仓库；不能把依赖 `/tmp` 文件的路径当作长期复现资产。

## 8. 已知限制与未验证范围

- 九宫格图形自身用于构造 stencil 遮罩时，当前主动回退到普通缩放。
  原因是嵌套的 stencil 遮罩构造无法表达本次逐片交集。
  九宫格图形被其他遮罩裁剪的情况已测试通过。
- API 中的 scale9Grid stub 标记暂时保留，不能据此判断此版本完全未实现，
  也不要在剩余兼容性工作完成前贸然去掉标记。
- 未修改包围盒报告和精确形状命中测试。复杂形状的视觉与命中一致性仍需验证。
- 标签读取已接入 AVM1/AVM2 Button，但按钮多状态、AVM2 合成状态容器、
  用户设置状态对象等组合尚无完整回归，不能宣称全面支持按钮九宫格。
- MorphShape 接入已实现，但复杂形变、动画期间网格变化尚缺专门图像测试。
- scrollRect、滤镜、多层混合、位图填充、父级旋转和斜切、极端缩放及视口
  非整数缩放的组合没有穷尽验证。完整游戏界面与浏览器端也未完成验证。
- 每个矢量对象最多绘制九次，并增加每片的 stencil 建立和清理。
  大量九宫格对象可能增加绘制命令、细分工作和样式变体缓存开销。

## 9. 后续排查顺序

1. 保存 SWF 来源、版本或哈希、character ID、截图和触发步骤。
   区分实例、Sprite 和内部 Shape，勿只记录编辑器显示名称。
2. 检查 `DefineScalingGrid` 或运行时 `scale9Grid` 是否存在，单位是否正确。
3. 记录网格归属对象和实际绘制对象，核对 `to_owner`、自身矩阵与父矩阵。
4. 检查上述回退条件，特别是旋转/斜切、中心剩余尺寸和 `drawing_mask()`。
5. 关闭 cacheAsBitmap、滤镜后对比，再检查网格缓存键和 Drawing 变体清理。
6. 若几何正确而底色不对，检查渐变补偿矩阵，勿重复补偿 Bitmap 填充。
7. 若只在 BitmapData.draw 出现，检查绘制根状态是否正确传入和恢复。
8. 对切片边界细缝记录 twips 端点与采样质量，比较单采样和多采样结果。
9. 提取最小 SWF，加上独立预期图或 Flash Player 参考后再修改算法。
   修改后至少重跑两组新增测试及对应的缓存/遮罩测试。

建议优先优化：为 stencil 遮罩引入可正确表达切片交集的几何方案；
补充真实 Flash Player 边界样本；为按钮、形变、scrollRect 和浏览器端增加
针对性用例；最后根据实测命令数与帧耗时决定是否改成几何切片或批量绘制。

## 10. 参考语义与分析工具

- [AIR DisplayObject.scale9Grid 文档](https://airsdk.dev/reference/actionscript/3.0/flash/display/DisplayObject.html#scale9Grid)
- [FFDec 26.3.0 发布包](https://github.com/jindrapetrik/jpexs-decompiler/releases/download/version26.3.0/ffdec_26.3.0.zip)

可用以下命令重新查看 SWF 标签数据，路径自行替换：

```sh
java -Djava.awt.headless=true -jar /path/to/ffdec.jar \
  -swf2xml /path/to/GreenGame.swf /tmp/GreenGame.xml
java -Djava.awt.headless=true -jar /path/to/ffdec.jar \
  -selectid 75 -export sprite /tmp/GreenGame-export /path/to/GreenGame.swf
```

服务器或无桌面环境运行 FFDec 时保留 `-Djava.awt.headless=true`，否则图像
导出可能因 X11 不可用而失败。单独导出 sprite 75 的原始图像只能确认素材，
不能验证实例缩放后的九宫格效果；必须结合放置矩阵或独立测试 SWF。
