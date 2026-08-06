/*
 * @Describe: ISO 8601 duration 手写解析 —— 把 BPMN `<timeDuration>PT1H30M</timeDuration>`
 * 解析成归一化秒数（TimerDuration）。
 *
 * 延续项目「受控手写解析器」哲学（如 expr.rs 不引 rhai）：不引第三方 duration 크레이트，
 * 手写一个覆盖审批场景所需子集的解析器，行为可控、依赖为零。
 *
 * 支持子集：`P[nD]T[nH][nM][nS]`，各段可缺省，至少一段非零。例：
 *   PT30S / PT10M / PT1H / PT1H30M / P1D / P1DT2H / P2DT3H4M5S
 * 不支持：周（nW）、月/年（nM 在 T 前的月歧义）、小数、负数——审批限时用不到，显式拒绝。
 */

use crate::error::{Error, Result};
use crate::ir::TimerDuration;

const SECS_PER_MINUTE: i64 = 60;
const SECS_PER_HOUR: i64 = 3600;
const SECS_PER_DAY: i64 = 86_400;

/// 解析 ISO 8601 duration（相对时长）字符串为归一化秒数。
///
/// 大小写不敏感于 `P`/`T` 与单位字母。空白被 trim。解析失败或全零返回错误。
pub fn parse_iso8601_duration(input: &str) -> Result<TimerDuration> {
    let s = input.trim();
    let bytes = s.as_bytes();
    if bytes.is_empty() || !(bytes[0] == b'P' || bytes[0] == b'p') {
        return Err(Error::InvalidDefinition(format!(
            "定时器时长 '{input}' 非法：应以 'P' 开头的 ISO 8601 duration"
        )));
    }

    let mut total: i64 = 0;
    let mut in_time = false; // 是否已越过 'T'（区分分钟 M 与月）
    let mut num_buf = String::new();
    let mut saw_any = false;

    for &b in &bytes[1..] {
        let c = b as char;
        match c {
            'T' | 't' => {
                if !num_buf.is_empty() {
                    return Err(Error::InvalidDefinition(format!(
                        "定时器时长 '{input}' 非法：'T' 前有悬挂数字"
                    )));
                }
                in_time = true;
            }
            '0'..='9' => num_buf.push(c),
            'D' | 'd' => {
                total += take_number(&mut num_buf, input, 'D')? * SECS_PER_DAY;
                saw_any = true;
            }
            'H' | 'h' => {
                if !in_time {
                    return Err(Error::InvalidDefinition(format!(
                        "定时器时长 '{input}' 非法：小时 'H' 必须在 'T' 之后"
                    )));
                }
                total += take_number(&mut num_buf, input, 'H')? * SECS_PER_HOUR;
                saw_any = true;
            }
            'M' | 'm' => {
                // T 之前的 M = 月（不支持）；T 之后的 M = 分钟。
                if !in_time {
                    return Err(Error::InvalidDefinition(format!(
                        "定时器时长 '{input}' 不支持月（'M' 在 'T' 前）；请用天/时/分/秒"
                    )));
                }
                total += take_number(&mut num_buf, input, 'M')? * SECS_PER_MINUTE;
                saw_any = true;
            }
            'S' | 's' => {
                if !in_time {
                    return Err(Error::InvalidDefinition(format!(
                        "定时器时长 '{input}' 非法：秒 'S' 必须在 'T' 之后"
                    )));
                }
                total += take_number(&mut num_buf, input, 'S')?;
                saw_any = true;
            }
            _ => {
                return Err(Error::InvalidDefinition(format!(
                    "定时器时长 '{input}' 含不支持的字符 '{c}'"
                )));
            }
        }
    }

    if !num_buf.is_empty() {
        return Err(Error::InvalidDefinition(format!(
            "定时器时长 '{input}' 非法：结尾有无单位的悬挂数字 '{num_buf}'"
        )));
    }
    if !saw_any || total <= 0 {
        return Err(Error::InvalidDefinition(format!(
            "定时器时长 '{input}' 必须为正且至少含一个时间段"
        )));
    }
    Ok(TimerDuration { seconds: total })
}

/// 取出累积的数字并清空缓冲；缓冲空表示单位前缺数字。
fn take_number(buf: &mut String, input: &str, unit: char) -> Result<i64> {
    if buf.is_empty() {
        return Err(Error::InvalidDefinition(format!(
            "定时器时长 '{input}' 非法：单位 '{unit}' 前缺数字"
        )));
    }
    let n: i64 = buf.parse().map_err(|_| {
        Error::InvalidDefinition(format!("定时器时长 '{input}' 的数字 '{buf}' 无法解析"))
    })?;
    buf.clear();
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_durations() {
        assert_eq!(parse_iso8601_duration("PT30S").unwrap().seconds, 30);
        assert_eq!(parse_iso8601_duration("PT10M").unwrap().seconds, 600);
        assert_eq!(parse_iso8601_duration("PT1H").unwrap().seconds, 3600);
        assert_eq!(parse_iso8601_duration("PT1H30M").unwrap().seconds, 5400);
        assert_eq!(parse_iso8601_duration("P1D").unwrap().seconds, 86_400);
        assert_eq!(
            parse_iso8601_duration("P1DT2H").unwrap().seconds,
            86_400 + 7200
        );
        assert_eq!(
            parse_iso8601_duration("P2DT3H4M5S").unwrap().seconds,
            2 * 86_400 + 3 * 3600 + 4 * 60 + 5
        );
    }

    #[test]
    fn trims_and_is_case_insensitive() {
        assert_eq!(parse_iso8601_duration("  pt1h  ").unwrap().seconds, 3600);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse_iso8601_duration("").is_err());
        assert!(parse_iso8601_duration("1H").is_err()); // 缺 P
        assert!(parse_iso8601_duration("PT").is_err()); // 全空
        assert!(parse_iso8601_duration("P0D").is_err()); // 零
        assert!(parse_iso8601_duration("PT1X").is_err()); // 非法单位
        assert!(parse_iso8601_duration("P1M").is_err()); // 月不支持
        assert!(parse_iso8601_duration("PTH").is_err()); // 单位前缺数字
    }
}
