// Date, time and email checks adapted from pi-schema@1.5.0 / TypeBox.
// Copyright (c) 2017-2026 Haydn Paterson
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
// THE SOFTWARE.

use std::sync::OnceLock;

use regex::Regex;

pub(crate) fn check_builtin_format(format: &str, value: &str) -> bool {
    match format {
        "uuid" => uuid::Uuid::parse_str(value).is_ok(),
        "uri" | "url" => url::Url::parse(value).is_ok(),
        "ipv4" => value.parse::<std::net::Ipv4Addr>().is_ok(),
        "ipv6" => value.parse::<std::net::Ipv6Addr>().is_ok(),
        "regex" => regress::Regex::with_flags(value, "u").is_ok(),
        "email" => check_email(value),
        "date" => check_date(value),
        "time" => check_time(value),
        "date-time" => value
            .split_once(['T', 't'])
            .is_some_and(|(date, time)| check_date(date) && check_time(time)),
        _ => true,
    }
}

fn check_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, b)| index == 4 || index == 7 || b.is_ascii_digit())
    {
        return false;
    }
    let year: u32 = value[..4].parse().expect("four ASCII digits");
    let month: usize = value[5..7].parse().expect("two ASCII digits");
    let day: u32 = value[8..].parse().expect("two ASCII digits");
    let leap_year =
        year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = [
        0,
        31,
        if leap_year { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    (1..=12).contains(&month) && day >= 1 && day <= days[month]
}

fn check_time(value: &str) -> bool {
    static TIME: OnceLock<Regex> = OnceLock::new();
    let regex = TIME.get_or_init(|| {
        Regex::new(r"\A([0-9]{2}):([0-9]{2}):([0-9]{2})(?:\.[0-9]+)?(?:[Zz]|([+-])([0-9]{2}):([0-9]{2}))\z")
            .expect("constant time pattern")
    });
    let Some(captures) = regex.captures(value) else {
        return false;
    };
    let number = |index| -> i32 {
        captures
            .get(index)
            .map_or(0, |value| value.as_str().parse().expect("two ASCII digits"))
    };
    let (hour, minute, second) = (number(1), number(2), number(3));
    let (offset_hour, offset_minute) = (number(5), number(6));
    if hour > 23 || minute > 59 || second > 60 || offset_hour > 23 || offset_minute > 59 {
        return false;
    }
    if second != 60 {
        return true;
    }
    let sign = if captures.get(4).is_some_and(|sign| sign.as_str() == "-") {
        -1
    } else {
        1
    };
    (hour * 60 + minute - sign * (offset_hour * 60 + offset_minute)).rem_euclid(1440) == 1439
}

fn check_email(value: &str) -> bool {
    static EMAIL: OnceLock<Regex> = OnceLock::new();
    EMAIL.get_or_init(|| {
        // Explicit ASCII letter ranges preserve ECMAScript non-Unicode /i folding.
        Regex::new(r#"\A(?:[a-zA-Z0-9!#$%&'*+/=?^_`{|}~-]+(?:\.[a-zA-Z0-9!#$%&'*+/=?^_`{|}~-]+)*|"(?:[^"\\]|\\[\x20-\x7e])*")@(?:[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)*|\[(?:[iI][pP][vV]6:[a-fA-F0-9:]+|(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])(?:\.(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])){3})\])\z"#)
            .expect("constant email pattern")
    }).is_match(value)
}
