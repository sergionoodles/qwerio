//! Decoding PostgreSQL binary values into core values.
//!
//! Known types decode exactly (numeric stays decimal text). Other types become
//! [`Value::Unsupported`] carrying their type name and raw wire bytes, never an
//! invented representation.

use std::error::Error;
use std::net::{Ipv4Addr, Ipv6Addr};

use fallible_iterator::FallibleIterator;
use postgres_protocol::types as protocol;
use qwerio_core::value::{Temporal, TemporalKind, UnsupportedValue, Value, format_float};
use tokio_postgres::types::{FromSql, Kind, Type};

/// Captures a cell's raw bytes regardless of its type.
pub(crate) struct RawCell<'a> {
    pub raw: Option<&'a [u8]>,
}

impl<'a> FromSql<'a> for RawCell<'a> {
    fn from_sql(_type: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn Error + Sync + Send>> {
        Ok(Self { raw: Some(raw) })
    }

    fn from_sql_null(_type: &Type) -> Result<Self, Box<dyn Error + Sync + Send>> {
        Ok(Self { raw: None })
    }

    fn accepts(_type: &Type) -> bool {
        true
    }
}

pub(crate) fn decode_row(row: &tokio_postgres::Row) -> Vec<Value> {
    row.columns()
        .iter()
        .enumerate()
        .map(
            |(index, column)| match row.try_get::<_, RawCell<'_>>(index) {
                Ok(cell) => decode_value(column.type_(), cell.raw),
                Err(error) => unsupported(column.type_(), None, Some(error.to_string())),
            },
        )
        .collect()
}

pub(crate) fn decode_value(type_: &Type, raw: Option<&[u8]>) -> Value {
    let Some(raw) = raw else {
        return Value::Null;
    };

    decode_present(type_, raw).unwrap_or_else(|| unsupported(type_, Some(raw), None))
}

fn unsupported(type_: &Type, raw: Option<&[u8]>, text: Option<String>) -> Value {
    Value::Unsupported(UnsupportedValue {
        type_name: type_.name().to_owned(),
        text,
        raw: raw.map(<[u8]>::to_vec),
    })
}

fn decode_present(type_: &Type, raw: &[u8]) -> Option<Value> {
    match type_.kind() {
        Kind::Enum(_) => return text(raw),
        Kind::Domain(base) => return Some(decode_value(base, Some(raw))),
        Kind::Array(member) => return decode_array(member, raw),
        _ => {}
    }

    let value = match *type_ {
        Type::BOOL => Value::Bool(protocol::bool_from_sql(raw).ok()?),
        Type::INT2 => Value::Integer(i64::from(protocol::int2_from_sql(raw).ok()?)),
        Type::INT4 => Value::Integer(i64::from(protocol::int4_from_sql(raw).ok()?)),
        Type::INT8 => Value::Integer(protocol::int8_from_sql(raw).ok()?),
        Type::OID => Value::Integer(i64::from(protocol::oid_from_sql(raw).ok()?)),
        // Through the shortest f32 text so 0.1::real reads as 0.1, not 0.100000001.
        Type::FLOAT4 => Value::Float(
            format!("{}", protocol::float4_from_sql(raw).ok()?)
                .parse()
                .ok()?,
        ),
        Type::FLOAT8 => Value::Float(protocol::float8_from_sql(raw).ok()?),
        Type::NUMERIC => Value::Decimal(decode_numeric(raw)?),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN | Type::XML => {
            return text(raw);
        }
        Type::CHAR => Value::Text(char::from(*raw.first()?).to_string()),
        Type::BYTEA => Value::Bytes(raw.to_vec()),
        Type::JSON => Value::Json(std::str::from_utf8(raw).ok()?.to_owned()),
        // JSONB's binary form is a version byte followed by the text.
        Type::JSONB => match raw.split_first() {
            Some((1, document)) => Value::Json(std::str::from_utf8(document).ok()?.to_owned()),
            _ => return None,
        },
        Type::UUID => Value::Uuid(format_uuid(raw)?),
        Type::DATE => temporal(
            TemporalKind::Date,
            format_date(protocol::date_from_sql(raw).ok()?),
        ),
        Type::TIME => temporal(
            TemporalKind::Time,
            format_time(protocol::time_from_sql(raw).ok()?),
        ),
        Type::TIMETZ => temporal(TemporalKind::TimeWithZone, format_time_with_zone(raw)?),
        Type::TIMESTAMP => temporal(
            TemporalKind::Timestamp,
            format_timestamp(protocol::timestamp_from_sql(raw).ok()?, false),
        ),
        Type::TIMESTAMPTZ => temporal(
            TemporalKind::TimestampWithZone,
            format_timestamp(protocol::timestamp_from_sql(raw).ok()?, true),
        ),
        Type::INTERVAL => temporal(TemporalKind::Interval, format_interval(raw)?),
        Type::INET | Type::CIDR => Value::Text(format_inet(raw)?),
        Type::MACADDR => Value::Text(format_mac(raw)?),
        _ => return None,
    };

    Some(value)
}

fn text(raw: &[u8]) -> Option<Value> {
    std::str::from_utf8(raw)
        .ok()
        .map(|text| Value::Text(text.to_owned()))
}

fn temporal(kind: TemporalKind, text: String) -> Value {
    Value::Temporal(Temporal { kind, text })
}

fn decode_array(member: &Type, raw: &[u8]) -> Option<Value> {
    let array = protocol::array_from_sql(raw).ok()?;
    let elements: Vec<Option<&[u8]>> = array.values().collect().ok()?;

    Some(Value::Array(
        elements
            .into_iter()
            .map(|element| decode_value(member, element))
            .collect(),
    ))
}

fn read_u16(raw: &[u8], offset: usize) -> Option<u16> {
    let bytes = raw.get(offset..offset + 2)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_i32(raw: &[u8], offset: usize) -> Option<i32> {
    let bytes: [u8; 4] = raw.get(offset..offset + 4)?.try_into().ok()?;
    Some(i32::from_be_bytes(bytes))
}

fn read_i64(raw: &[u8], offset: usize) -> Option<i64> {
    let bytes: [u8; 8] = raw.get(offset..offset + 8)?.try_into().ok()?;
    Some(i64::from_be_bytes(bytes))
}

const NUMERIC_NEGATIVE: u16 = 0x4000;
const NUMERIC_NAN: u16 = 0xC000;
const NUMERIC_POSITIVE_INFINITY: u16 = 0xD000;
const NUMERIC_NEGATIVE_INFINITY: u16 = 0xF000;
const NUMERIC_GROUP_DIGITS: usize = 4;

/// Exact decimal text from PostgreSQL's base-10000 numeric format.
pub(crate) fn decode_numeric(raw: &[u8]) -> Option<String> {
    let group_count = usize::from(read_u16(raw, 0)?);
    let weight = i32::from(read_u16(raw, 2)? as i16);
    let sign = read_u16(raw, 4)?;
    let scale = usize::from(read_u16(raw, 6)?);

    match sign {
        NUMERIC_NAN => return Some("NaN".to_owned()),
        NUMERIC_POSITIVE_INFINITY => return Some("Infinity".to_owned()),
        NUMERIC_NEGATIVE_INFINITY => return Some("-Infinity".to_owned()),
        _ => {}
    }

    let groups: Vec<u16> = (0..group_count)
        .map(|index| read_u16(raw, 8 + index * 2))
        .collect::<Option<_>>()?;
    let group_at = |index: i32| -> u16 {
        usize::try_from(index)
            .ok()
            .and_then(|index| groups.get(index).copied())
            .unwrap_or(0)
    };

    let mut text = String::new();

    if sign == NUMERIC_NEGATIVE {
        text.push('-');
    }

    if weight < 0 {
        text.push('0');
    } else {
        text.push_str(&group_at(0).to_string());

        for index in 1..=weight {
            text.push_str(&format!("{:04}", group_at(index)));
        }
    }

    if scale > 0 {
        let fraction_groups = scale.div_ceil(NUMERIC_GROUP_DIGITS);
        let mut fraction = String::with_capacity(fraction_groups * NUMERIC_GROUP_DIGITS);

        for offset in 0..fraction_groups {
            fraction.push_str(&format!("{:04}", group_at(weight + 1 + offset as i32)));
        }

        fraction.truncate(scale);
        text.push('.');
        text.push_str(&fraction);
    }

    Some(text)
}

fn format_uuid(raw: &[u8]) -> Option<String> {
    if raw.len() != 16 {
        return None;
    }

    let hex: String = raw.iter().map(|byte| format!("{byte:02x}")).collect();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

/// Days between 1970-01-01 and PostgreSQL's epoch, 2000-01-01.
const POSTGRES_EPOCH_UNIX_DAYS: i64 = 10_957;
const MICROS_PER_SECOND: i64 = 1_000_000;
const MICROS_PER_DAY: i64 = 86_400 * MICROS_PER_SECOND;

/// Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_unix_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);

    (year, month, day)
}

fn format_civil_date(postgres_days: i64) -> String {
    let (year, month, day) = civil_from_unix_days(postgres_days + POSTGRES_EPOCH_UNIX_DAYS);

    if year <= 0 {
        format!("{:04}-{month:02}-{day:02} BC", 1 - year)
    } else {
        format!("{year:04}-{month:02}-{day:02}")
    }
}

fn format_date(days: i32) -> String {
    match days {
        i32::MAX => "infinity".to_owned(),
        i32::MIN => "-infinity".to_owned(),
        days => format_civil_date(i64::from(days)),
    }
}

/// `HH:MM:SS` with a fractional part only when present, as PostgreSQL prints.
fn format_time(micros: i64) -> String {
    let seconds = micros.div_euclid(MICROS_PER_SECOND);
    let fraction = micros.rem_euclid(MICROS_PER_SECOND);
    let text = format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        seconds / 60 % 60,
        seconds % 60
    );

    if fraction == 0 {
        return text;
    }

    let digits = format!("{fraction:06}");
    format!("{text}.{}", digits.trim_end_matches('0'))
}

fn format_zone_offset(seconds_east: i32) -> String {
    let sign = if seconds_east < 0 { '-' } else { '+' };
    let absolute = seconds_east.unsigned_abs();
    let (hours, minutes, seconds) = (absolute / 3_600, absolute / 60 % 60, absolute % 60);

    match (minutes, seconds) {
        (0, 0) => format!("{sign}{hours:02}"),
        (_, 0) => format!("{sign}{hours:02}:{minutes:02}"),
        _ => format!("{sign}{hours:02}:{minutes:02}:{seconds:02}"),
    }
}

fn format_time_with_zone(raw: &[u8]) -> Option<String> {
    let micros = read_i64(raw, 0)?;
    // PostgreSQL stores the zone as seconds west of UTC.
    let seconds_west = read_i32(raw, 8)?;
    Some(format!(
        "{}{}",
        format_time(micros),
        format_zone_offset(-seconds_west)
    ))
}

fn format_timestamp(micros: i64, with_zone: bool) -> String {
    match micros {
        i64::MAX => return "infinity".to_owned(),
        i64::MIN => return "-infinity".to_owned(),
        _ => {}
    }

    let days = micros.div_euclid(MICROS_PER_DAY);
    let time = micros.rem_euclid(MICROS_PER_DAY);
    let date = format_civil_date(days);

    // Era suffixes go after the time, as in PostgreSQL output.
    let (date, era) = match date.strip_suffix(" BC") {
        Some(date) => (date.to_owned(), " BC"),
        None => (date, ""),
    };
    let zone = if with_zone { "+00" } else { "" };

    format!("{date} {}{zone}{era}", format_time(time))
}

fn format_interval(raw: &[u8]) -> Option<String> {
    let micros = read_i64(raw, 0)?;
    let days = read_i32(raw, 8)?;
    let months = read_i32(raw, 12)?;

    let mut parts = Vec::new();
    let (years, months) = (months / 12, months % 12);

    let mut push_unit = |amount: i32, singular: &str, plural: &str| {
        if amount != 0 {
            let unit = if amount.abs() == 1 { singular } else { plural };
            parts.push(format!("{amount} {unit}"));
        }
    };

    push_unit(years, "year", "years");
    push_unit(months, "mon", "mons");
    push_unit(days, "day", "days");

    if micros != 0 || parts.is_empty() {
        let sign = if micros < 0 { "-" } else { "" };
        parts.push(format!("{sign}{}", format_time(micros.abs())));
    }

    Some(parts.join(" "))
}

const INET_FAMILY_V4: u8 = 2;
const INET_FAMILY_V6: u8 = 3;

fn format_inet(raw: &[u8]) -> Option<String> {
    let (&family, rest) = raw.split_first()?;
    let (&bits, rest) = rest.split_first()?;
    let (&is_cidr, rest) = rest.split_first()?;
    let (&length, address) = rest.split_first()?;

    if address.len() != usize::from(length) {
        return None;
    }

    let (text, max_bits) = match family {
        INET_FAMILY_V4 => {
            let octets: [u8; 4] = address.try_into().ok()?;
            (Ipv4Addr::from(octets).to_string(), 32)
        }
        INET_FAMILY_V6 => {
            let octets: [u8; 16] = address.try_into().ok()?;
            (Ipv6Addr::from(octets).to_string(), 128)
        }
        _ => return None,
    };

    if is_cidr != 0 || bits != max_bits {
        return Some(format!("{text}/{bits}"));
    }

    Some(text)
}

fn format_mac(raw: &[u8]) -> Option<String> {
    if raw.len() != 6 {
        return None;
    }

    Some(
        raw.iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(":"),
    )
}

/// Text accepted by PostgreSQL's input functions for a bound value. `None`
/// binds SQL NULL.
pub(crate) fn value_to_text(value: &Value) -> Result<Option<String>, String> {
    let text = match value {
        Value::Null => return Ok(None),
        Value::Float(number) => format_float(*number),
        Value::Unsupported(unsupported) => match &unsupported.text {
            Some(text) => text.clone(),
            None => {
                return Err(format!(
                    "A {} value without text cannot be used as a parameter",
                    unsupported.type_name
                ));
            }
        },
        other => other.canonical_text().unwrap_or_default(),
    };

    Ok(Some(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numeric_bytes(weight: i16, sign: u16, scale: u16, groups: &[u16]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend((groups.len() as u16).to_be_bytes());
        bytes.extend(weight.to_be_bytes());
        bytes.extend(sign.to_be_bytes());
        bytes.extend(scale.to_be_bytes());

        for group in groups {
            bytes.extend(group.to_be_bytes());
        }

        bytes
    }

    #[test]
    fn numerics_decode_exactly() {
        // 12345678.9012 = groups [1234, 5678, 9012], weight 1, scale 4
        assert_eq!(
            decode_numeric(&numeric_bytes(1, 0, 4, &[1234, 5678, 9012])).as_deref(),
            Some("12345678.9012")
        );
        // -0.000123 = groups [1, 2300], weight -1, scale 6
        assert_eq!(
            decode_numeric(&numeric_bytes(-1, NUMERIC_NEGATIVE, 6, &[1, 2300])).as_deref(),
            Some("-0.000123")
        );
        // 1000000 with trailing zero groups omitted
        assert_eq!(
            decode_numeric(&numeric_bytes(1, 0, 0, &[100])).as_deref(),
            Some("1000000")
        );
        assert_eq!(
            decode_numeric(&numeric_bytes(0, NUMERIC_NAN, 0, &[])).as_deref(),
            Some("NaN")
        );
    }

    #[test]
    fn dates_and_timestamps_match_postgres_output() {
        assert_eq!(format_date(0), "2000-01-01");
        assert_eq!(format_date(-730_119), "0001-01-01");
        assert_eq!(format_date(-730_120), "0001-12-31 BC");
        assert_eq!(format_timestamp(1_500_000, false), "2000-01-01 00:00:01.5");
        assert_eq!(format_timestamp(-1, true), "1999-12-31 23:59:59.999999+00");
        assert_eq!(format_timestamp(i64::MAX, false), "infinity");
    }

    #[test]
    fn intervals_use_postgres_units() {
        let mut raw = Vec::new();
        raw.extend(3_723_000_000_i64.to_be_bytes());
        raw.extend(2_i32.to_be_bytes());
        raw.extend(14_i32.to_be_bytes());

        assert_eq!(
            format_interval(&raw).as_deref(),
            Some("1 year 2 mons 2 days 01:02:03")
        );
    }

    #[test]
    fn time_zones_print_as_offsets_east_of_utc() {
        let mut raw = Vec::new();
        raw.extend(0_i64.to_be_bytes());
        raw.extend((-19_800_i32).to_be_bytes());

        assert_eq!(
            format_time_with_zone(&raw).as_deref(),
            Some("00:00:00+05:30")
        );
    }

    #[test]
    fn network_addresses_format_like_postgres() {
        assert_eq!(
            format_inet(&[2, 32, 0, 4, 10, 0, 0, 1]).as_deref(),
            Some("10.0.0.1")
        );
        assert_eq!(
            format_inet(&[2, 24, 1, 4, 10, 0, 0, 0]).as_deref(),
            Some("10.0.0.0/24")
        );
    }

    #[test]
    fn unknown_types_keep_raw_bytes() {
        let value = decode_value(&Type::POINT, Some(&[1, 2, 3]));

        assert_eq!(
            value,
            Value::Unsupported(UnsupportedValue {
                type_name: "point".to_owned(),
                text: None,
                raw: Some(vec![1, 2, 3]),
            })
        );
    }

    #[test]
    fn parameters_bind_canonical_text() {
        assert_eq!(value_to_text(&Value::Null), Ok(None));
        assert_eq!(
            value_to_text(&Value::Bytes(vec![0xab])),
            Ok(Some("\\xab".to_owned()))
        );
        assert_eq!(
            value_to_text(&Value::Float(f64::INFINITY)),
            Ok(Some("Infinity".to_owned()))
        );
    }
}
