//! Basic check which just verifies that everything compiles
use satrs_core::res_code::ResultU16;
use satrs_mib::resultcode;

#[resultcode]
const _TEST_RESULT: ResultU16 = ResultU16::const_new(0, 1);

fn main() {}
