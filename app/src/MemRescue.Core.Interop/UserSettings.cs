using System.Text.Json;
using System.Text.Json.Serialization;

namespace MemRescue.Core.Interop;

/// <summary>Preferencias persistentes del usuario.</summary>
public sealed record UserSettings
{
    /// <summary>Relleno de los sectores ilegibles.</summary>
    public FillPattern BadFill { get; init; } = FillPattern.DefaultBad;

    /// <summary>Relleno de los sectores saltados.</summary>
    public FillPattern SkipFill { get; init; } = FillPattern.DefaultSkip;

    public static UserSettings Defaults { get; } = new();
}

/// <summary>
/// Fichero de preferencias por usuario (JSON). Ubicación: <c>%APPDATA%\MemRescue\settings.json</c>, o el
/// directorio de la variable de entorno <see cref="DirectoryVariable"/> (las pruebas la usan para no
/// tocar nunca el perfil real). Nunca se escribe junto a las imágenes abiertas. Un fichero ausente, dañado
/// o con valores no admitidos se ignora: se usan los valores por defecto y no se sobrescribe hasta que el
/// usuario guarde.
/// </summary>
public sealed class SettingsStore(string path)
{
    /// <summary>Variable de entorno con el directorio de preferencias que sustituye al del perfil.</summary>
    public const string DirectoryVariable = "MEMRESCUE_SETTINGS_DIR";

    public const string FileName = "settings.json";

    /// <summary>Tamaño máximo admitido del fichero (evita leer ficheros desmesurados).</summary>
    public const int MaxFileBytes = 64 * 1024;

    private const int FormatVersion = 1;

    public string Path { get; } = System.IO.Path.GetFullPath(path);

    /// <summary>Almacén en la ubicación por defecto (o la de <see cref="DirectoryVariable"/>).</summary>
    public static SettingsStore ForCurrentUser() =>
        new(ResolvePath(Environment.GetEnvironmentVariable(DirectoryVariable),
            Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData)));

    /// <summary>Ruta del fichero: el directorio indicado por la variable si no está vacío; si no, el del perfil.</summary>
    public static string ResolvePath(string? overrideDirectory, string appData) =>
        string.IsNullOrWhiteSpace(overrideDirectory)
            ? System.IO.Path.Combine(appData, "MemRescue", FileName)
            : System.IO.Path.Combine(overrideDirectory, FileName);

    /// <summary>
    /// Lee las preferencias. Nunca lanza: si el fichero no existe devuelve los valores por defecto y
    /// <paramref name="warning"/> nulo; si está dañado o sus valores no son admisibles, los valores por
    /// defecto y el motivo en <paramref name="warning"/>.
    /// </summary>
    public UserSettings Load(out string? warning)
    {
        warning = null;
        try
        {
            if (!File.Exists(Path))
            {
                return UserSettings.Defaults;
            }
            var info = new FileInfo(Path);
            if (info.Length > MaxFileBytes)
            {
                warning = $"El fichero de preferencias es demasiado grande ({info.Length:N0} bytes); se usan los valores por defecto.";
                return UserSettings.Defaults;
            }
            var dto = JsonSerializer.Deserialize(File.ReadAllText(Path), SettingsJsonContext.Default.SettingsDto);
            if (dto is null)
            {
                warning = "El fichero de preferencias está vacío; se usan los valores por defecto.";
                return UserSettings.Defaults;
            }
            if (!FillPattern.TryParse(dto.BadFill ?? FillPattern.DefaultBad.ToHex(), out var bad, out var parseError)
                || !FillPattern.TryParse(dto.SkipFill ?? FillPattern.DefaultSkip.ToHex(), out var skip, out parseError))
            {
                warning = $"Patrón de relleno no válido en las preferencias; se usan los valores por defecto. {parseError}";
                return UserSettings.Defaults;
            }
            if (FillPattern.Validate(bad, skip) is { } invalid)
            {
                warning = $"Patrones de relleno no admitidos en las preferencias; se usan los valores por defecto. {invalid}";
                return UserSettings.Defaults;
            }
            return new UserSettings { BadFill = bad, SkipFill = skip };
        }
        catch (Exception ex) when (ex is JsonException or IOException or UnauthorizedAccessException or NotSupportedException)
        {
            warning = $"No se pudo leer el fichero de preferencias ({ex.Message}); se usan los valores por defecto.";
            return UserSettings.Defaults;
        }
    }

    /// <summary>
    /// Guarda las preferencias (escritura atómica: fichero temporal y sustitución). Lanza
    /// <see cref="ArgumentException"/> si los patrones no son admisibles.
    /// </summary>
    public void Save(UserSettings settings)
    {
        ArgumentNullException.ThrowIfNull(settings);
        if (FillPattern.Validate(settings.BadFill, settings.SkipFill) is { } invalid)
        {
            throw new ArgumentException(invalid, nameof(settings));
        }
        var dir = System.IO.Path.GetDirectoryName(Path)!;
        Directory.CreateDirectory(dir);
        var dto = new SettingsDto
        {
            Version = FormatVersion,
            BadFill = settings.BadFill.ToHex(),
            SkipFill = settings.SkipFill.ToHex(),
        };
        var tmp = Path + ".tmp";
        File.WriteAllText(tmp, JsonSerializer.Serialize(dto, SettingsJsonContext.Default.SettingsDto));
        File.Move(tmp, Path, overwrite: true);
    }
}

/// <summary>Forma persistente: patrones como 8 dígitos hexadecimales (sin ambigüedad ASCII/hex).</summary>
internal sealed class SettingsDto
{
    [JsonPropertyName("version")]
    public int Version { get; set; }

    [JsonPropertyName("bad_fill")]
    public string? BadFill { get; set; }

    [JsonPropertyName("skip_fill")]
    public string? SkipFill { get; set; }
}

[JsonSourceGenerationOptions(WriteIndented = true)]
[JsonSerializable(typeof(SettingsDto))]
internal sealed partial class SettingsJsonContext : JsonSerializerContext;
