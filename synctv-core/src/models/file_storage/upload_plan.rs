//! Overflow-safe arithmetic for resumable upload plans.

#[cfg(verus_only)]
use vstd::prelude::*;

#[derive(Debug, Clone)]
#[cfg_attr(not(verus_only), derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(verus_only, verus_verify)]
pub struct FileUploadPlanPart {
    pub part_number: i32,
    pub offset_bytes: i64,
    pub size_bytes: i64,
}

#[derive(Debug, Clone)]
#[cfg_attr(not(verus_only), derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(not(verus_only), serde(rename_all = "camelCase"))]
#[cfg_attr(verus_only, verus_verify)]
pub struct FileUploadManifestPart {
    pub part_number: i32,
    pub offset_bytes: i64,
    pub size_bytes: i64,
    pub checksum_sha256: String,
}

/// Return the number of fixed-size parts needed to cover a positive payload.
#[cfg_attr(verus_only, verus_spec(result => requires size_bytes > 0, part_size_bytes > 0, ensures
    result == (size_bytes as int - 1) / part_size_bytes as int + 1,
    1 <= result <= size_bytes,
    (result as int - 1) * part_size_bytes < size_bytes <= result as int * part_size_bytes,
))]
fn upload_part_count(size_bytes: i64, part_size_bytes: i64) -> i64 {
    #[cfg(verus_only)]
    proof! {
        let n = size_bytes as int - 1;
        let d = part_size_bytes as int;
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod(n, d);
        assert((n / d + 1) * d == (n / d) * d + d) by (nonlinear_arith);
    }
    (size_bytes - 1) / part_size_bytes + 1
}

#[cfg(verus_only)]
verus! {
    pub(crate) closed spec fn planned(part: FileUploadPlanPart, index: int, total: int, width: int) -> bool {
        &&& part.part_number == index + 1
        &&& part.offset_bytes == index * width
        &&& 0 <= part.offset_bytes < total
        &&& part.size_bytes == vstd::math::min(width, total - part.offset_bytes)
    }
}

/// The same one-based layout is used for new plans and for completing stored sessions.
#[cfg_attr(verus_only, verus_spec(out => ensures
    out.is_some() == (number > 0 && width > 0 && (number as int - 1) * width < total),
    out matches Some(part) ==> planned(part, number as int - 1, total as int, width as int),
))]
pub(crate) fn planned_part(total: i64, width: i64, number: i32) -> Option<FileUploadPlanPart> {
    if number <= 0 || width <= 0 {
        return None;
    }
    let offset_bytes = i64::from(number - 1).checked_mul(width)?;
    if offset_bytes >= total {
        return None;
    }
    Some(FileUploadPlanPart {
        part_number: number,
        offset_bytes,
        size_bytes: width.min(total - offset_bytes),
    })
}

#[cfg_attr(verus_only, verus_spec(out => ensures
    out.is_some() == (total > 0 && width > 0 && total <= i32::MAX as int * width),
    out matches Some(parts) ==> (
        parts.len() == (total as int - 1) / width as int + 1 &&
        (forall|i: int| 0 <= i < parts.len() ==> planned(#[trigger] parts@[i], i, total as int, width as int)) &&
        parts@[0].offset_bytes == 0 &&
        (forall|i: int| 0 <= i < parts.len() - 1 ==> #[trigger] parts@[i].offset_bytes + parts@[i].size_bytes == parts@[i + 1].offset_bytes) &&
        parts@[parts.len() - 1].offset_bytes + parts@[parts.len() - 1].size_bytes == total),
))]
pub(crate) fn plan_parts(total: i64, width: i64) -> Option<Vec<FileUploadPlanPart>> {
    if total <= 0 || width <= 0 {
        return None;
    }
    let count = upload_part_count(total, width);
    #[cfg(verus_only)]
    proof! {
        assert((count <= i32::MAX) == (total <= i32::MAX as int * width)) by (nonlinear_arith)
            requires width > 0, (count as int - 1) * width < total <= count as int * width;
    }
    let count = i32::try_from(count).ok()?;
    let mut parts = Vec::with_capacity(usize::try_from(count).ok()?);
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            0 <= index <= count,
            count > 0, width > 0,
            (count as int - 1) * width < total <= count as int * width,
            parts.len() == index,
            forall|i: int| 0 <= i < index ==> planned(#[trigger] parts@[i], i, total as int, width as int),
        decreases count - index,
    ))]
    while index < count {
        #[cfg(verus_only)]
        proof! {
            assert(index as int * width < total) by (nonlinear_arith)
                requires index < count, width > 0, (count as int - 1) * width < total;
        }
        index += 1;
        parts.push(planned_part(total, width, index)?);
    }
    #[cfg(verus_only)]
    proof! {
        assert forall|i: int| 0 <= i < count - 1 implies
            #[trigger] parts@[i].offset_bytes + parts@[i].size_bytes == parts@[i + 1].offset_bytes by {
            assert(planned(parts@[i + 1], i + 1, total as int, width as int));
            assert((i + 1) * width == i * width + width) by (nonlinear_arith);
        }
        assert(planned(parts@[count - 1], count - 1, total as int, width as int));
        assert((count as int - 1) * width + width == count as int * width) by (nonlinear_arith);
    }
    Some(parts)
}

#[cfg(verus_only)]
verus! {
    pub(crate) closed spec fn matches_at(parts: Seq<FileUploadManifestPart>, i: int, total: int, width: int) -> bool {
        let p = parts[i];
        &&& p.part_number == i + 1
        &&& p.offset_bytes == (if i == 0 { 0int } else { parts[i - 1].offset_bytes + parts[i - 1].size_bytes })
        &&& 0 <= p.offset_bytes < total
        &&& p.size_bytes == vstd::math::min(width, total - p.offset_bytes)
    }
}

/// Check the submitted layout in one pass without allocating a second plan.
#[cfg_attr(verus_only, verus_spec(ok => ensures ok == (
    total > 0 && width > 0 && parts.len() > 0 &&
    (forall|i: int| 0 <= i < parts.len() ==> #[trigger] matches_at(parts@, i, total as int, width as int)) &&
    parts@.last().offset_bytes + parts@.last().size_bytes == total),
))]
pub(crate) fn manifest_matches_plan(
    total: i64,
    width: i64,
    parts: &[FileUploadManifestPart],
) -> bool {
    if total <= 0 || width <= 0 || parts.is_empty() {
        return false;
    }
    let mut offset = 0;
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= parts.len(), 0 <= offset <= total, width > 0,
            offset == (if index == 0 { 0int } else { parts@[index - 1].offset_bytes + parts@[index - 1].size_bytes }),
            forall|i: int| 0 <= i < index ==> #[trigger] matches_at(parts@, i, total as int, width as int),
        decreases parts.len() - index,
    ))]
    while index < parts.len() {
        let part = &parts[index];
        let size = width.min(total - offset);
        let number = usize::try_from(part.part_number).ok();
        #[cfg(verus_only)]
        proof! {
            assert((number == Some((index + 1) as usize)) == (part.part_number == index as int + 1));
            assert((size > 0 && part.part_number == index as int + 1 && part.offset_bytes == offset && part.size_bytes == size)
                == matches_at(parts@, index as int, total as int, width as int));
        }
        if size == 0
            || number != Some(index + 1)
            || part.offset_bytes != offset
            || part.size_bytes != size
        {
            return false;
        }
        offset += size;
        index += 1;
    }
    offset == total
}

#[cfg_attr(verus_only, verus_verify)]
pub(crate) enum RangeError {
    Bounds,
    Payload,
    Alignment,
    Width,
    Index,
    Number,
}

/// Validate against the part width captured when the upload session was opened.
#[cfg_attr(verus_only, verus_spec(out => ensures
    out.is_ok() == (0 <= start <= end < total && total == expected && width > 0 &&
        end as int - start + 1 == payload && start as int % width as int == 0 &&
        end as int + 1 == vstd::math::min(start as int + width, total as int) &&
        start as int / (width as int) < i32::MAX),
    out matches Ok(part) ==> (
        planned(part, start as int / width as int, total as int, width as int) &&
        part.offset_bytes == start && part.size_bytes == payload && part.part_number > 0),
))]
pub(crate) fn part_for_range(
    start: i64,
    end: i64,
    total: i64,
    expected: i64,
    width: i64,
    payload: usize,
) -> Result<FileUploadPlanPart, RangeError> {
    if start < 0 || end < start || total != expected || end >= total || width <= 0 {
        return Err(RangeError::Bounds);
    }
    let length = end - start + 1;
    if usize::try_from(length).ok() != Some(payload) {
        return Err(RangeError::Payload);
    }
    if start % width != 0 {
        return Err(RangeError::Alignment);
    }
    if length != width.min(total - start) {
        return Err(RangeError::Width);
    }
    let Ok(index) = i32::try_from(start / width) else {
        return Err(RangeError::Index);
    };
    let Some(number) = index.checked_add(1) else {
        return Err(RangeError::Number);
    };
    #[cfg(verus_only)]
    proof! {
        let d = width as int;
        let n = start as int;
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod(n, d);
        assert((n / d) * d == d * (n / d)) by (nonlinear_arith);
        assert((n / d) * d == n);
    }
    Ok(FileUploadPlanPart {
        part_number: number,
        offset_bytes: start,
        size_bytes: length,
    })
}
