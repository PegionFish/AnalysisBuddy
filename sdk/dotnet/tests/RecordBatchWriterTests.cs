// D2-01 DoD tests: RecordBatchWriter batching, seq continuity, done:true final
// batch, ~1MiB early flush, NaN/±∞ drop with stderr count, heartbeat, and
// ThrowIfCancelled.

using System.Text.Json;
using AnalysisBuddy.Sdk;
using AnalysisBuddy.Sdk.Errors;
using Xunit;

namespace AnalysisBuddy.Sdk.Tests;

public class RecordBatchWriterTests
{
    private sealed class CapturedFrames
    {
        public List<RpcNotification> Frames { get; } = new();
        public SemaphoreSlim Signal { get; } = new(0, int.MaxValue);

        public Task SendAsync(object frame)
        {
            lock (Frames)
            {
                Frames.Add((RpcNotification)frame);
            }

            Signal.Release();
            return Task.CompletedTask;
        }

        public RpcNotification WaitFor(string method, int timeoutMs = 5000)
        {
            var deadline = DateTime.UtcNow.AddMilliseconds(timeoutMs);
            while (DateTime.UtcNow < deadline)
            {
                lock (Frames)
                {
                    var match = Frames.LastOrDefault(f => f.Method == method);
                    if (match is not null)
                    {
                        return match;
                    }
                }

                Thread.Sleep(10);
            }

            throw new TimeoutException($"no {method} frame within {timeoutMs}ms");
        }
    }

    private static RecordBatchWriter CreateWriter(string fileId, CapturedFrames frames, int batchSize = 4000, TimeSpan? heartbeat = null)
        => new(fileId, frames.SendAsync, batchSize, heartbeat ?? TimeSpan.FromSeconds(2));

    [Theory]
    [InlineData(999)]
    [InlineData(8001)]
    public void Ctor_RejectsOutOfRangeBatchSize(int batchSize)
    {
        var frames = new CapturedFrames();
        Assert.Throws<ArgumentOutOfRangeException>(() => CreateWriter("f", frames, batchSize));
    }

    [Theory]
    [InlineData(1000)]
    [InlineData(8000)]
    public void Ctor_AcceptsBoundaryBatchSize(int batchSize)
    {
        var frames = new CapturedFrames();
        _ = CreateWriter("f", frames, batchSize);
    }

    [Fact]
    public async Task Seq_StartsAtZero_NoGaps_AfterFlush()
    {
        var frames = new CapturedFrames();
        var writer = CreateWriter("file-1", frames, batchSize: 1000);
        for (ulong i = 0; i < 2500; i++)
        {
            await writer.EmitAsync(new Record((long)i, "fps", 60.0));
        }

        await writer.FlushAsync();

        var batches = frames.Frames.Where(f => f.Method == "RecordBatch").ToList();
        Assert.Equal(3, batches.Count); // 1000 + 1000 + 500

        var seqs = batches.Select(f => ((RecordBatchParams)f.Params).Seq).ToArray();
        Assert.Equal(new ulong[] { 0, 1, 2 }, seqs);

        var counts = batches.Select(f => ((RecordBatchParams)f.Params).Records.Count).ToArray();
        Assert.Equal(new[] { 1000, 1000, 500 }, counts);

        Assert.False(((RecordBatchParams)batches[0].Params).Done);
        Assert.False(((RecordBatchParams)batches[1].Params).Done);
        Assert.True(((RecordBatchParams)batches[2].Params).Done);
    }

    [Fact]
    public async Task FlushAsync_EmitsDoneTrueFinalBatch_EvenWhenEmpty()
    {
        var frames = new CapturedFrames();
        var writer = CreateWriter("file-1", frames);
        await writer.FlushAsync();

        var batch = (RecordBatchParams)frames.Frames.Single(f => f.Method == "RecordBatch").Params;
        Assert.True(batch.Done);
        Assert.Empty(batch.Records);
    }

    [Fact]
    public async Task EarlyFlush_NearOneMiB_FlushesBeforeBatchSize()
    {
        var frames = new CapturedFrames();
        // 700 records × ~1.5KB raw_line ≈ 1.05MB → early flush before batchSize 4000
        var writer = CreateWriter("file-1", frames, batchSize: 4000);
        var rawLine = new string('x', 1500);
        for (int i = 0; i < 700; i++)
        {
            await writer.EmitAsync(new Record(i, "fps", 60.0).WithRawLine(rawLine));
        }

        await writer.FlushAsync();

        var batches = frames.Frames.Where(f => f.Method == "RecordBatch").ToList();
        Assert.True(batches.Count >= 2, $"expected early flush, got {batches.Count} batches");
        Assert.True(((RecordBatchParams)batches[0].Params).Records.Count < 4000);
    }

    [Theory]
    [InlineData(double.NaN)]
    [InlineData(double.PositiveInfinity)]
    [InlineData(double.NegativeInfinity)]
    public async Task NonFiniteValues_AreDroppedAndCounted(double value)
    {
        var frames = new CapturedFrames();
        var writer = CreateWriter("file-1", frames);
        var before = writer.DroppedNonFinite;
        await writer.EmitAsync(new Record(1, "fps", value));
        Assert.Equal(before + 1, writer.DroppedNonFinite);
        Assert.Equal(0UL, writer.RecordsEmitted);
    }

    [Fact]
    public async Task Heartbeat_SilentPeriod_EmitsProgressAutomatically()
    {
        var frames = new CapturedFrames();
        var writer = CreateWriter("file-1", frames, heartbeat: TimeSpan.FromMilliseconds(50));
        using var cts = new CancellationTokenSource();
        var heartbeat = writer.HeartbeatLoopAsync(cts.Token);

        await writer.EmitAsync(new Record(1, "fps", 60.0));
        var progress = frames.WaitFor("progress", timeoutMs: 5000);
        var p = (ProgressParams)progress.Params;
        Assert.Equal("file-1", p.FileId);
        Assert.Equal(1UL, p.RecordsSoFar);
        Assert.Null(p.Percent);   // heartbeat omits percent/bytes_read
        Assert.Null(p.BytesRead);

        cts.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => heartbeat);
    }

    [Fact]
    public async Task ThrowIfCancelled_ThrowsCancelledExceptionAfterCancel()
    {
        var frames = new CapturedFrames();
        var writer = CreateWriter("file-1", frames);
        using var cts = new CancellationTokenSource();
        writer.AttachCancellation(cts.Token);
        cts.Cancel();
        Assert.Throws<CancelledException>(() => writer.ThrowIfCancelled());
    }

    [Fact]
    public async Task ThrowIfCancelled_NoopBeforeCancel()
    {
        var frames = new CapturedFrames();
        var writer = CreateWriter("file-1", frames);
        writer.ThrowIfCancelled(); // must not throw
    }
}
