# 07 · C# SDK 教程与 API 摘要（AnalysisBuddy.Sdk）

> 契约依据：`AnalysisBuddy-devdocs/deep-dive/sdk-plugins.md` §2（SDK 设计）；
> 协议行为以 [docs/spec/protocol-v1.md](../spec/protocol-v1.md) 为准。
> 本文件只摘要 API 签名与关键行为；与 Python SDK 逐条对齐的行为不再重复展开，
> 见 `06-sdk-python.md`。

## 安装与形态

```powershell
dotnet add package AnalysisBuddy.Sdk
```

- TFM 下限 net8.0（更新版本可直接引用）；**零 PackageReference**
  （仅 BCL 内建 `System.Text.Json`）；
- 序列化：`DefaultIgnoreCondition = WhenWritingNull` + 集合空即省略，落实
  skip-if-empty（protocol-v1.md §3.1）；
- 包结构：`PluginHost.cs` / `IPluginHandler.cs` / `RecordBatchWriter.cs` /
  `PluginErrors.cs` / `NdjsonTransport.cs` / `Models.cs`（协议 POCO，字段
  snake_case 用 `JsonPropertyName` 显式标注）。

## 接口：`IPluginHandler`

```csharp
public interface IPluginHandler
{
    PluginInfo Info { get; }   // record PluginInfo(string Id, string Name, string Version);

    Task<CanHandleResult> CanHandleAsync(CanHandleParams p, CancellationToken ct);      // §2.2
    Task<FileSummary?>    LoadFileAsync(LoadFileParams p, CancellationToken ct);         // §2.3，返回 null = 空 {}
    Task<ulong>           ParseAsync(string fileId, JsonElement? options,
                                     RecordBatchWriter writer, CancellationToken ct);    // §2.4
    Task<SchemaResult>    SchemaAsync(CancellationToken ct);                             // §2.5
    Task<KeyValuesResult> KeyValuesAsync(string fileId, long timestampMs, CancellationToken ct); // §2.6
    Task<AnnotateResult>  AnnotateAsync(string fileId, TimeRange range, CancellationToken ct);   // §2.7，可选
    Task                  UnloadFileAsync(string fileId, CancellationToken ct);          // §2.8
}
```

| 成员 | 说明 |
|------|------|
| `Info` | 元数据（initialize 响应素材，§2.1） |
| `CanHandleAsync` | 文件认领探测（§2.2） |
| `LoadFileAsync` | 加载文件（§2.3）；返回 `null` 表示空 `{}` |
| `ParseAsync` | 流式解析（§2.4）：数据经 `writer` 回传，返回 `records_total`（ulong） |
| `SchemaAsync` | 指标声明（§2.5） |
| `KeyValuesAsync` | 时刻快照（§2.6） |
| `AnnotateAsync` | 事件标注（§2.7，可选） |
| `UnloadFileAsync` | 幂等卸载（§2.8） |

> capabilities 自动推导：覆写了 `AnnotateAsync` 即 `annotate=true`；
> `subscribe`/`binary_sidecar` 恒为 false。

作者继承抽象基类 `PluginHandlerBase`（提供全部默认实现：`CanHandleAsync` 弃权、
`AnnotateAsync` 抛 `-32005`、`UnloadFileAsync` 空操作），只覆写需要的方法。

## 主循环：`PluginHost.RunAsync`

```csharp
public static class PluginHost
{
    public static Task RunAsync(IPluginHandler handler, CancellationToken ct = default);
    public static Task RunAsync(IPluginHandler handler, Stream stdin, Stream stdout,
                                TextWriter? stderrLog = null, CancellationToken ct = default);
}
```

行为契约与 Python `serve()` 完全对齐：stdin 逐行读（行长度先于内容校验、
超限即退出）、stdout 整行原子写（`SemaphoreSlim` 发送锁）、stderr 日志
（`PluginLog.Info/Warn/Error` 前缀分级）、**stdin EOF → flush → 退出码 0**、
`shutdown`/`cancel_parse` 自动应答、同 `file_id` 并发 parse 自动回 `-32001`、
未知方法回 `-32601`；方法路由与 Python SDK 相同的 10 个 method。

## 批量与心跳：`RecordBatchWriter`

```csharp
public sealed class RecordBatchWriter : IAsyncDisposable
{
    public RecordBatchWriter(string fileId, /* Host 注入发送器 */
                             int batchSize = 4000);   // 合法区间以 sdk-plugins.md §2.4 为准，越界 throw

    public Task EmitAsync(Record record, CancellationToken ct = default);
    public Task EmitAsync(IEnumerable<Record> records, CancellationToken ct = default);
    public Task ProgressAsync(double? percent = null, ulong? bytesRead = null,
                              CancellationToken ct = default);
    public void ThrowIfCancelled();              // 等价 Python ctx.check_cancelled()
    public Task FlushAsync();                // on_parse 返回后 Host 自动调：flush 残余 + done:true 末批
}
```

批量（默认与合法区间见 sdk-plugins.md §2.4）、体积接近协议建议上限时提前 flush、
解析期间周期心跳（Host 内置 `PeriodicTimer`）、末批 `done:true`、`records_total`
校验义务——与 Python SDK 逐条一致；`Record.value` 为 NaN/±∞ 时丢弃并 stderr 计数。

## 异常 → 错误码映射（`AnalysisBuddy.Sdk.Errors`）

| 异常 | code | 名称 |
|------|------|------|
| `PluginBusyException` | `-32001` | plugin_busy |
| `FileLoadFailedException` | `-32002` | file_load_failed |
| `ParseFailedException` | `-32003` | parse_failed（`Data` 属性进 error.data） |
| `CancelledException` | `-32004` | cancelled |
| `UnsupportedInV1Exception` | `-32005` | unsupported_in_v1 |
| `InvalidParamsException` | `-32602` | Invalid params |
| 兜底 | `-32603` | Internal error |

Host 序列化错误对象时硬校验 code 集合，`-32001`~`-32005` 之外的自定义 code 被拒绝。

## 最小插件（示意级）

```csharp
using AnalysisBuddy.Sdk;

sealed class HelloHandler : PluginHandlerBase
{
    public override PluginInfo Info => new("hello-plugin", "Hello", "0.1.0");

    public override Task<CanHandleResult> CanHandleAsync(CanHandleParams p, CancellationToken ct)
        => Task.FromResult(new CanHandleResult(p.Ext == "log", 0.9, null));

    public override async Task<ulong> ParseAsync(string fileId, JsonElement? options,
                                                 RecordBatchWriter writer, CancellationToken ct)
    {
        ulong total = 0;
        foreach (var line in File.ReadLines(_paths[fileId]))
        {
            writer.ThrowIfCancelled();
            var (ts, v) = ParseLine(line);
            await writer.EmitAsync(new Record(ts, "demo", v));
            total++;
        }
        return total;
    }
}

await PluginHost.RunAsync(new HelloHandler());   // 顶层语句即插件入口
```

---

📌 章节要点（双视角）

👤 **给人**：C# SDK 适合「已有 C# 代码库」的场景——`PluginHandlerBase` 基类把所有
协议机械活（路由/心跳/帧纪律）都做了，作者只覆写业务方法；发布用
`dotnet publish -c Release -o publish`，`plugin.json` 的 `entry` 指向
`publish/<project>.exe`（写法见 `04-manifest-reference.md`）。

🤖 **给 Agent**：BEH 自证清单与 Python SDK 相同（见 `06-sdk-python.md` 章节要点）：
Host 已保证 BEH-02/03/04/06/08/09/10/12；作者侧须自证 BEH-01（`Info.Id` 与
manifest 一致）、BEH-05（`Record.Metric` ∈ `SchemaAsync()` 声明）。API 签名以本
文件与 sdk-plugins.md §2.2 为准，不得臆造方法。
