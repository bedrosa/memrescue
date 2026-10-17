using MemRescue.Core.Interop;
using Xunit;

namespace MemRescue.Core.Interop.Tests;

public sealed class HexDumpTests
{
    [Fact]
    public void FormatsAbsoluteOffsetsHexAndAscii()
    {
        var data = "EFI PART"u8.ToArray().Concat(new byte[] { 0, 0, 1, 0, 0x5C, 0, 0, 0 }).ToArray();
        var text = HexDump.Format(data, 0x200);
        Assert.Equal("000000000200  45 46 49 20 50 41 52 54  00 00 01 00 5C 00 00 00  EFI PART....\\...\n", text);
    }

    [Fact]
    public void PartialLastLineKeepsColumnsAligned()
    {
        var text = HexDump.Format(new byte[] { 0x41, 0x42, 0x43 }, 0);
        Assert.StartsWith("000000000000  41 42 43 ", text);
        Assert.EndsWith("  ABC\n", text);
        Assert.Equal(text.IndexOf("ABC", StringComparison.Ordinal), HexDump.Format(new byte[16], 0).IndexOf("................", StringComparison.Ordinal));
    }

    [Fact]
    public void LinesPerSector()
    {
        foreach (var ss in Engine.CommonSectorSizes)
        {
            var text = HexDump.Format(new byte[ss], 0);
            Assert.Equal((int)ss / 16, text.Count(c => c == '\n'));
        }
    }

    [Theory]
    [InlineData("1234", 1234ul)]
    [InlineData("0x1F", 31ul)]
    [InlineData("1Fh", 31ul)]
    [InlineData(" 32_768 ", 32768ul)]
    [InlineData("1.048.576", 1048576ul)]
    [InlineData("0xFFFFFFFFFFFFFFFF", ulong.MaxValue)]
    public void ParsesDecimalAndHex(string input, ulong expected)
    {
        Assert.True(HexDump.TryParseNumber(input, out var v));
        Assert.Equal(expected, v);
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("-1")]
    [InlineData("12abc")]
    [InlineData("0x")]
    [InlineData("0x1FFFFFFFFFFFFFFFF")]
    public void RejectsInvalidNumbers(string? input)
    {
        Assert.False(HexDump.TryParseNumber(input, out _));
    }
}
