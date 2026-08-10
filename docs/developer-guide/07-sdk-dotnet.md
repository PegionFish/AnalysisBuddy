# 07 · C# SDK 教程与 API 摘要（AnalysisBuddy.Sdk）

> 契约依据：`AnalysisBuddy-devdocs/deep-dive/sdk-plugins.md` §2（SDK 设计）；
> 协议行为以 [docs/spec/protocol-v1.md](../spec/protocol-v1.md) 为准。
> 本文件只摘要 API 签名与关键行为；与 Python SDK 逐条对齐的行为不再重复展开，
> 见 `06-sdk-python.md`。

## 安装与形态

SDK 源码在 `sdk/dotnet/AnalysisBuddy.Sdk.csproj`，当前以项目引用方式使用
（尚未发布 NuGet；发布后等价于 `dotnet add package AnalysisBuddy.Sdk`）：

```xml
<!-- 你的插件 csproj 里（路径按仓库布局调整） -->
<ItemGroup>
  <ProjectReference Include="..\..\sdk\dotnet\AnalysisBuddy.Sdk.csproj" />
</ItemGroup>
```

- TFM 下限 net8.0（更新版本可直接引用）；**零 PackageReference**
  （仅 BCL 内建 `System.Text.Json`）；
- 序列化：`DefaultIgnoreCondition = WhenWritingNull` + 集合空即省略，落实
  skip-if-empty（protocol-v1.md §3.1）；
- 包结构：`PluginHost.cs` / `IPluginHandler.cs` / `RecordBatchWriter.cs` /
  `PluginErrors.cs` / `NdjsonTransport.cs` / `Models.cs`（协议 POCO，字段
  snake_case 用 `JsonPropertyName` 显式标注）；
- 仓库内合规样例：`sdk/dotnet/examples/sample-plugin-csharp`（与 Python 样例
  行为同构，复制整个目录即可作为新插件起步）。

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

> capabilities 自动推导：覆写了 `AnnotateAsync` 即 `annotate=true`（直接实现
> `IPluginHandler` 接口的按已实现计）；`subscribe`/`binary_sidecar` 恒为 false。

作者继承抽象基类 `PluginHandlerBase`（提供默认实现：`CanHandleAsync` 弃权、
`AnnotateAsync` 抛 `-32005`、`UnloadFileAsync` 空操作），只覆写需要的方法；
注意 `Info`/`LoadFileAsync`/`ParseAsync`/`SchemaAsync`/`KeyValuesAsync` 在基类
中是 abstract，必须实现。

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
    public const int MinBatchSize = 1000;
    public const int MaxBatchSize = 8000;

    // 作者不自行构造：传给 ParseAsync 的 writer 由 PluginHost 创建并注入发送器；
    // 公开构造（fileId, batchSize = 4000）仅供测试，未附着发送器时 Emit 会抛异常。

    public Task EmitAsync(Record record, CancellationToken ct = default);
    public Task EmitAsync(IEnumerable<Record> records, CancellationToken ct = default);
    public Task ProgressAsync(double? percent = null, ulong? bytesRead = null,
                              CancellationToken ct = default);
    public void ThrowIfCancelled();              // 等价 Python ctx.check_cancelled()
    public Task FlushAsync();                // on_parse 返回后 Host 自动调：flush 残余 + done:true 末批
}
```

批量（缺省 4000，合法区间 1000~8000，构造时越界 throw）、累计序列化体积接近
900 KB 时提前 flush、解析期间周期心跳（Host 内置 `PeriodicTimer`，2s）、
末批 `done:true`、`records_total` 校验义务——与 Python SDK 逐条一致；
`Record.value` 为 NaN/±∞ 时丢弃并 stderr 计数。

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

## 最小插件（完整可运行）

下面是最小但**完整可运行**的 C# 插件（顶层语句入口）。它解析表头为
`timestamp,fps` 的极简 CSV——仓库内合规样例
`sdk/dotnet/examples/sample-plugin-csharp/Program.cs` 的同构精简版：

```csharp
using System.Text.Json;
using AnalysisBuddy.Sdk;
using AnalysisBuddy.Sdk.Errors;

// 顶层语句即插件入口（进程从这里启动）
await PluginHost.RunAsync(new HelloHandler());

sealed class HelloHandler : PluginHandlerBase
{
    private readonly Dictionary<string, string> _paths = new();

    public override PluginInfo Info => new("hello-plugin", "Hello", "0.1.0");

    public override Task<CanHandleResult> CanHandleAsync(CanHandleParams p, CancellationToken ct)
        => Task.FromResult(new CanHandleResult(p.Ext == "log", 0.9, null));

    public override Task<FileSummary?> LoadFileAsync(LoadFileParams p, CancellationToken ct)
    {
        if (!File.Exists(p.Path))
        {
            throw new FileLoadFailedException($"file not found: {p.Path}");
        }
        _paths[p.FileId] = p.Path;
        return Task.FromResult<FileSummary?>(null);   // null = 空 {} 摘要
    }

    public override async Task<ulong> ParseAsync(string fileId, JsonElement? options,
                                                 RecordBatchWriter writer, CancellationToken ct)
    {
        ulong total = 0;
        foreach (var line in File.ReadLines(_paths[fileId]).Skip(1))   // 跳过表头
        {
            writer.ThrowIfCancelled();
            var parts = line.Split(',');
            if (parts.Length < 2 ||
                !long.TryParse(parts[0], out var ts) ||
                !double.TryParse(parts[1], out var fps))
            {
                throw new ParseFailedException($"malformed row: {line}");
            }
            await writer.EmitAsync(new Record(ts, "fps", fps), ct);
            total++;
        }
        return total;
    }

    public override Task<SchemaResult> SchemaAsync(CancellationToken ct)
        => Task.FromResult(new SchemaResult(new List<MetricDef>
        {
            new("fps", "FPS", "fps", "frames per second", Aggregation.Last),
        }));

    public override Task<KeyValuesResult> KeyValuesAsync(string fileId, long timestampMs,
                                                         CancellationToken ct)
        => Task.FromResult(new KeyValuesResult(new List<KeyValueEntry>()));
}
```

> `PluginHandlerBase` 的五个 abstract 成员（`Info`/`LoadFileAsync`/`ParseAsync`/
> `SchemaAsync`/`KeyValuesAsync`）缺一不可，否则无法编译；`UnloadFileAsync`
> 基类默认空操作，持有 `_paths` 这类状态时建议覆写做清理。

配套 `plugin.json`（entry 指向发布产物，`id` 必须与目录名、`Info.Id` 三方一致）：

```json
{
  "id": "hello-plugin",
  "display_name": "Hello",
  "version": "0.1.0",
  "entry": { "command": "publish/HelloPlugin.exe" },
  "match": { "extensions": ["log"] }
}
```

构建与自检（PowerShell，仓库根执行）：

```powershell
dotnet publish .\hello-plugin\HelloPlugin.csproj -c Release -o .\hello-plugin\publish
& .\tools\plugin-validator\target\release\plugin-validator.exe check .\hello-plugin --behavior --fixture .\sample.log
echo $LASTEXITCODE   # 期望 0
```

完整行为同构样例（含 `ProgressAsync` 进度上报与 `ParseFailedException` 携带
`error.data`）见 `sdk/dotnet/examples/sample-plugin-csharp/`，复制整个目录改 id
即可作为新插件起步。

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
