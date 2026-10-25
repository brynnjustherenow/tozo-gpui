# tozo-gpui

[English](./README.en.md) | 简体中文

#### 本项目是tozo的gpui重写版

[freya ver](https://github.com/brynnjustherenow/tozo.git)

原本是想继续用freya实现lua做配置文件的，但是看到gpui-shell可以用js做插件，还是决定迁移到gpui了

#### 目前已实现

- [x] 支持smtc的播放源显示

- [x] 当smtc来自bilibili时，支持拉取弹幕在桌面上显示（到底谁会用得到？）

- [x] smtc媒体播放控制

- [x] 时间显示

- [x] cpu、内存、网络监控（应用列表可按cpu/ram排序）

- [x] 音量、亮度控制，电量显示

- [x] 监控系统端口占用以及控制

- [x] 日志功能 

#### 计划实现

- [ ] 使用 ~~lua~~ js配置组件布局和外观
- [ ] 使用~~lua~~ js脚本自定义组件
- [ ] 添加天气组件
- [ ] 对于youtube等平台支持拉取字幕显示
- [ ] 自定义的bar背景图片
- [ ] 全屏应用启动时退化为fps等性能检测
- [ ] 增加quickjs配置自定义样式
- [ ] 接管alt+tab
- [ ] 用js自定义组件
- [ ] i18n

#### 使用bilibili扩展加载桌面弹幕

1. 在chrome浏览器中打开开发者模式
2. 选择加载`未打包的扩展程序`选择browser extension
3.  默认游客身份只能获取少量弹幕，在exe目录下放入bilibili.tcookie文件，填入网页上获取的cookie即可获取全量弹幕（如果弹幕多的话会互相覆盖，我准备后续在lua配置文件中加入弹幕显示的配置，到时候再修复这一问题）
