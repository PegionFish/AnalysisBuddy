# SamplePluginCSharp — C# 合规样例插件

用 `AnalysisBuddy.Sdk` 编写的合规样例插件，与 D1-02 的 Python 样例插件同构（同一 CSV 格式、同一指标集合、同一 key_values 语义），用于跨 SDK 一致性验证（E 路「一致性总验收」前置）。

## 构建与发布

```powershell
cd sdk/dotnet/examples/sample-plugin-csharp
dotnet publish -c Release -o publish
# 可选单文件自包含发布（终端用户零 .NET 运行时依赖）：
# dotnet publish -c Release -o publish -p:PublishSingleFile=true --self-contained
```

`plugin.json` 的 `entry.command: "publish/SamplePluginCSharp.exe"` 指向标准 `dotnet publish` 产物（§5.2 布局），路径相对 `plugin.json` 目录解析。

## 结构校验与行为回放

```powershell
# 结构校验（MAN-01~MAN-09）
plugin check .\sdk\dotnet\examples\sample-plugin-csharp\

# 全量校验（结构 + 行为回放，21 条规则，退出码 0 为通过线）
plugin check .\sdk\dotnet\examples\sample-plugin-csharp\ --behavior --fixture .\sample.csv --json
echo $LASTEXITCODE   # 期望 0
```

## 与 Python 样例的判定一致性

两样例插件除语言与入口形态外行为完全一致，`plugin check --behavior` 的 21 条规则判定结论相同：

| 规则组 | 行为要点 | 两样例实现 |
|--------|----------|------------|
| MAN-01/02/03 | manifest 必填、id 与目录名一致、entry 产物存在 | `id: sample-plugin-csharp`；`publish/SamplePluginCSharp.exe` |
| BEH-01/02 | initialize 元数据齐全、id 一致、响应 id 匹配 | SDK 自动应答 |
| BEH-03 | 必选方法不回 `-32601` | 全部由 SDK 路由 |
| BEH-04 | parse 心跳 | SDK `RecordBatchWriter` 2s 心跳 + 每行 progress |
| BEH-05 | Record 必填字段 + metric ∈ schema + skip-if-empty | SDK 序列化自动生效 |
| BEH-06 | seq 无缺号、records_total 等于各批之和 | SDK seq 自 0 单调递增，末批 `done:true` |
| BEH-07 | key_values 结构合规 | 返回 scene/state 两条 entry |
| BEH-10/12 | shutdown 3s 内退出码 0、stdin EOF 自行退出 | SDK EOF 自杀 + shutdown 自动应答 |

## 冒烟测试

```powershell
dotnet test sdk/dotnet/tests/SamplePluginCSharp.Tests
```

冒烟测试拉起 `publish/` 产物跑最小序列（initialize → schema → can_handle → load_file → parse → key_values → unload_file → shutdown），并断言：

- shutdown 后 3s 内进程退出码 0（BEH-10）
- stdin 关闭后进程自行退出（BEH-12）

## 复制即起步

把 `sdk/dotnet/examples/sample-plugin-csharp/` 整个目录复制为你的插件仓库，改 `plugin.json` 与 handler 即可；`Info.Id` 必须与目录名一致（MAN-02），`entry.command` 指向 `dotnet publish` 产物。
