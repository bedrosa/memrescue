using System.Runtime.InteropServices;
using MemRescue.Core.Interop;
using Xunit;

namespace MemRescue.Core.Interop.Tests;

/// <summary>
/// Política de omisión de errores de la verificación (IO-09) a través de
/// <c>mr_source_verify_start_ex</c>. La semántica con errores simulados se prueba en Rust
/// (mr-io y mr-ffi); aquí se comprueba el contrato desde .NET contra la DLL real.
/// </summary>
public sealed class VerifyPolicyTests : IDisposable
{
    private readonly string _dir = Directory.CreateTempSubdirectory("mr-policy-").FullName;

    public void Dispose()
    {
        GC.Collect();
        GC.WaitForPendingFinalizers();
        Directory.Delete(_dir, recursive: true);
    }

    private string Numbered(string name, int sectors)
    {
        var p = Path.Combine(_dir, name);
        File.WriteAllBytes(p, Enumerable.Range(0, sectors * 512).Select(i => (byte)(i / 512)).ToArray());
        return p;
    }

    [Fact]
    public void OptionsStructMatchesHeader()
    {
        Assert.Equal(48, Marshal.SizeOf<MrVerifyOptions>());
    }

    [Fact]
    public async Task SkipCodesAndIgnoreAllAreAcceptedOnHealthyImage()
    {
        using var src = Engine.OpenSource(Numbered("p.img", 500));
        using (var t = src.StartVerify(retries: 3, skipErrorCodes: [23, 1117, 23]))
        {
            Assert.Equal(EngineTaskState.Completed, await t.WaitAsync());
            var r = t.GetReport();
            Assert.Equal((500ul, 500ul, 0ul), (r.SectorsRequested, r.SectorsRead, r.RetryReads));
            Assert.True(r.AllTrustworthy);
            Assert.Equal(0ul, r.OmittedErrors);
            Assert.Empty(r.ErrorCodes);
        }
        using (var t = src.StartVerify(startLba: 100, count: 50, jumpSectors: 16, readBack: true, ignoreAllErrors: true))
        {
            TaskProgress last = default;
            Assert.Equal(EngineTaskState.Completed, await t.WaitAsync(new SyncProgress(x => last = x)));
            Assert.Equal(new TaskProgress(50, 50), last);
            var r = t.GetReport();
            Assert.Equal((50ul, 50ul), (r.SectorsRequested, r.SectorsRead));
            Assert.Empty(r.UntrustedRanges);
        }
        // Lista vacía y sin «ignorar todo»: se usa la función clásica, mismo resultado.
        using (var t = src.StartVerify(skipErrorCodes: []))
        {
            Assert.Equal(EngineTaskState.Completed, await t.WaitAsync());
            Assert.Equal(500ul, t.GetReport().SectorsRead);
        }
    }

    [Fact]
    public async Task InvalidPolicyIsRejectedByTheEngine()
    {
        using var src = Engine.OpenSource(Numbered("q.img", 10));
        Assert.Equal(EngineStatus.InvalidArgument,
            Assert.Throws<EngineException>(() => src.StartVerify(skipErrorCodes: [23, -1])).Status);
        Assert.Equal(EngineStatus.InvalidArgument,
            Assert.Throws<EngineException>(() => src.StartVerify(skipErrorCodes: [0])).Status);
        Assert.Equal(EngineStatus.InvalidArgument,
            Assert.Throws<EngineException>(() => src.StartVerify(skipErrorCodes: Enumerable.Range(1, 257).ToArray())).Status);
        Assert.Equal(EngineStatus.InvalidArgument,
            Assert.Throws<EngineException>(() => src.StartVerify(retries: 101, ignoreAllErrors: true)).Status);
        // 256 códigos es el máximo admitido.
        using var t = src.StartVerify(skipErrorCodes: Enumerable.Range(1, 256).ToArray());
        Assert.Equal(EngineTaskState.Completed, await t.WaitAsync());
        // Rango fuera del origen: la tarea falla.
        using var f = src.StartVerify(startLba: 8, count: 5, ignoreAllErrors: true);
        Assert.Equal(EngineTaskState.Failed, await f.WaitAsync());
    }

    [Theory]
    [InlineData("", new int[0])]
    [InlineData("23", new[] { 23 })]
    [InlineData("1117, 23", new[] { 23, 1117 })]
    [InlineData(" 23;1117 23\t0x45D ", new[] { 23, 1117 })]
    [InlineData("0x17,5", new[] { 5, 23 })]
    public void ErrorCodeListParsesSortsAndDeduplicates(string text, int[] expected)
    {
        Assert.True(ErrorCodeList.TryParse(text, out var codes, out var error));
        Assert.Null(error);
        Assert.Equal(expected, codes);
    }

    [Theory]
    [InlineData("23,abc")]
    [InlineData("-5")]
    [InlineData("0")]
    [InlineData("99999999999")]
    [InlineData("0x")]
    public void ErrorCodeListRejectsInvalidEntries(string text)
    {
        Assert.False(ErrorCodeList.TryParse(text, out var codes, out var error));
        Assert.Empty(codes);
        Assert.Contains("no válido", error);
    }

    [Fact]
    public void ErrorCodeListRejectsTooManyCodesAndFormats()
    {
        var many = string.Join(",", Enumerable.Range(1, 257));
        Assert.False(ErrorCodeList.TryParse(many, out _, out var error));
        Assert.Contains("256", error);
        Assert.True(ErrorCodeList.TryParse(string.Join(",", Enumerable.Range(1, 256)), out var ok, out _));
        Assert.Equal(256, ok.Length);
        Assert.Equal("23, 1117", ErrorCodeList.Format([23, 1117]));
    }

    private sealed class SyncProgress(Action<TaskProgress> f) : IProgress<TaskProgress>
    {
        public void Report(TaskProgress value) => f(value);
    }
}
