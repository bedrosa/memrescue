using System.Buffers.Binary;

namespace MemRescue.TestSupport;

/// <summary>
/// VHD sintéticos según la especificación pública de Microsoft (enteros big-endian) para las pruebas de discos
/// diferenciales (incidencia 8): un disco fijo con id único y un diferencial dinámico enlazado a ese id. Se comparte
/// (enlazado) entre las pruebas de interoperabilidad y las de interfaz.
/// </summary>
internal static class SyntheticVhd
{
    private static void Seal(byte[] b, int field)
    {
        uint sum = 0;
        for (var i = 0; i < b.Length; i++)
        {
            if (i < field || i >= field + 4)
            {
                sum += b[i];
            }
        }
        BinaryPrimitives.WriteUInt32BigEndian(b.AsSpan(field), ~sum);
    }

    private static byte[] Footer(uint diskType, ulong size, ulong dataOffset, byte uniqueIdByte)
    {
        var f = new byte[512];
        "conectix"u8.CopyTo(f);
        BinaryPrimitives.WriteUInt32BigEndian(f.AsSpan(8), 2);
        BinaryPrimitives.WriteUInt32BigEndian(f.AsSpan(12), 0x0001_0000);
        BinaryPrimitives.WriteUInt64BigEndian(f.AsSpan(16), dataOffset);
        "mrts"u8.CopyTo(f.AsSpan(28));
        BinaryPrimitives.WriteUInt64BigEndian(f.AsSpan(40), size);
        BinaryPrimitives.WriteUInt64BigEndian(f.AsSpan(48), size);
        BinaryPrimitives.WriteUInt32BigEndian(f.AsSpan(60), diskType);
        f.AsSpan(68, 16).Fill(uniqueIdByte);
        Seal(f, 64);
        return f;
    }

    /// <summary>VHD fijo con id único formado por <paramref name="uniqueIdByte"/> repetido.</summary>
    public static byte[] Fixed(byte[] data, byte uniqueIdByte) =>
        [.. data, .. Footer(2, (ulong)data.Length, ulong.MaxValue, uniqueIdByte)];

    /// <summary>
    /// VHD diferencial de <paramref name="size"/> bytes (bloques de 4 KiB) cuyo padre tiene el id
    /// <paramref name="parentIdByte"/> repetido y se llama <paramref name="parentName"/>. Solo escribe los sectores
    /// de <paramref name="written"/>; los demás sectores de un bloque asignado llevan basura (0xEE) con su bit a 0.
    /// </summary>
    public static byte[] Differencing(ulong size, byte uniqueIdByte, byte parentIdByte, string parentName,
        IReadOnlyDictionary<int, byte> written)
    {
        const int bs = 4096;
        var entries = (int)((size + bs - 1) / bs);
        var batLen = (entries * 4 + 511) / 512 * 512;
        var locator = System.Text.Encoding.Unicode.GetBytes(".\\" + parentName);
        var locatorLen = (locator.Length + 511) / 512 * 512;
        var footer = Footer(4, size, 512, uniqueIdByte);
        var header = new byte[1024];
        "cxsparse"u8.CopyTo(header);
        BinaryPrimitives.WriteUInt64BigEndian(header.AsSpan(8), ulong.MaxValue);
        BinaryPrimitives.WriteUInt64BigEndian(header.AsSpan(16), 1536);
        BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(24), 0x0001_0000);
        BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(28), (uint)entries);
        BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(32), bs);
        header.AsSpan(40, 16).Fill(parentIdByte);
        var name = System.Text.Encoding.BigEndianUnicode.GetBytes(parentName);
        name.CopyTo(header.AsSpan(64));
        "W2ru"u8.CopyTo(header.AsSpan(576));
        BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(580), (uint)(locatorLen / 512));
        BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(584), (uint)locator.Length);
        BinaryPrimitives.WriteUInt64BigEndian(header.AsSpan(592), (ulong)(1536 + batLen));
        Seal(header, 36);
        var bat = Enumerable.Repeat((byte)0xFF, batLen).ToArray();
        var body = new List<byte>();
        var bodyStart = 1536 + batLen + locatorLen;
        foreach (var block in written.Keys.Select(s => s / 8).Distinct().Order())
        {
            BinaryPrimitives.WriteUInt32BigEndian(bat.AsSpan(block * 4), (uint)((bodyStart + body.Count) / 512));
            var bitmap = new byte[512];
            var data = Enumerable.Repeat((byte)0xEE, bs).ToArray();
            for (var k = 0; k < 8; k++)
            {
                if (written.TryGetValue(block * 8 + k, out var value))
                {
                    bitmap[0] |= (byte)(0x80 >> k);
                    data.AsSpan(k * 512, 512).Fill(value);
                }
            }
            body.AddRange(bitmap);
            body.AddRange(data);
        }
        var loc = new byte[locatorLen];
        locator.CopyTo(loc, 0);
        return [.. footer, .. header, .. bat, .. loc, .. body, .. footer];
    }
}
