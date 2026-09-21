//! Opt-in diagnostics for an explicitly supplied Account Tracker backup.
use means_core::imports::account_tracker::*;

#[test]
#[ignore = "requires an explicitly supplied MEANS_ATB backup; may print private data"]
fn at_debug() {
    let path = std::env::var_os("MEANS_ATB").expect("set MEANS_ATB to an explicit backup path");
    let content = std::fs::read(path).expect("read explicitly selected backup");
    let b = parse(&content).unwrap();
    for forward in [true, false] {
        NEGATIVE_APPLIES_FORWARD.store(forward, std::sync::atomic::Ordering::Relaxed);
        let sim = simulate(&b, PenceSide::To, true);
        let mism: Vec<String> = b
            .accounts
            .iter()
            .filter(|a| sim.get(&a.id).map(|(bal, _)| *bal != a.balance).unwrap_or(true))
            .map(|a| format!("{} {:+.2}", a.name, (sim.get(&a.id).map(|x| x.0).unwrap_or(0) - a.balance) as f64 / 100.0))
            .collect();
        eprintln!("negative-forward={forward}: {} mismatches: {}", mism.len(), mism.join(" | "));
    }
    NEGATIVE_APPLIES_FORWARD.store(false, std::sync::atomic::Ordering::Relaxed);
    for side in [PenceSide::From, PenceSide::To] {
        for expand in [true, false] {
            let sim = simulate(&b, side, expand);
            let mism = b.accounts.iter().filter(|a| sim.get(&a.id).map(|(bal, _)| *bal != a.balance).unwrap_or(true)).count();
            let cnt_mism = b.accounts.iter().filter(|a| sim.get(&a.id).map(|(_, n)| *n != a.todo).unwrap_or(true)).count();
            eprintln!("side {:?} expand {}: {} balance mismatches, {} count mismatches", side, expand, mism, cnt_mism);
        }
    }
    let sim_t = simulate(&b, PenceSide::To, true);
    let sim_f = simulate(&b, PenceSide::To, false);
    eprintln!("{:<22} {:>10} {:>10} {:>10} | {:>6} {:>6} {:>6}", "account", "expected", "expand", "noexpand", "todo", "n_exp", "n_no");
    for a in &b.accounts {
        let (be, ne) = sim_t.get(&a.id).copied().unwrap_or((0, 0));
        let (bn, nn) = sim_f.get(&a.id).copied().unwrap_or((0, 0));
        eprintln!("{:<22} {:>10.2} {:>10.2} {:>10.2} | {:>6} {:>6} {:>6}", a.name, a.balance as f64 / 100.0, be as f64 / 100.0, bn as f64 / 100.0, a.todo, ne, nn);
    }
    let rec: Vec<&AtTransaction> = b.transactions.iter().filter(|t| t.repeat.is_some()).collect();
    eprintln!("recurring rows: {}", rec.len());
    let name = |id: i64| b.accounts.iter().find(|a| a.id == id).map(|a| a.name.clone()).unwrap_or_else(|| id.to_string());
    for t in rec.iter() {
        let r = t.repeat.as_ref().unwrap();
        let occ = occurrences(t, b.exported_on);
        eprintln!(
            "  R {} {:<18} {:>8} {:>16}->{:<16} {:<8} every={} end={:<6} after={:<3} on={:?} wk={:<7} ov={} -> {} occ",
            t.date,
            t.details.chars().take(18).collect::<String>(),
            t.pence,
            name(t.from),
            name(t.to),
            r.unit,
            r.every,
            r.end,
            r.after,
            r.on,
            r.weekend,
            r.overrides.len(),
            occ.len()
        );
    }
    for t in rec.iter().filter(|t| !t.repeat.as_ref().unwrap().overrides.is_empty()) {
        let r = t.repeat.as_ref().unwrap();
        let occ = occurrences(t, b.exported_on);
        let seqs: Vec<i64> = occ.iter().map(|o| o.0).collect();
        eprintln!(
            "  OV {} {:<14} {:>7} {:<16} end={} after={} ov={} -> seqs {:?}",
            t.date,
            t.details.chars().take(14).collect::<String>(),
            t.pence,
            name(if t.from != 0 { t.from } else { t.to }),
            r.end,
            r.after,
            r.overrides.len(),
            seqs
        );
    }
    let all_occ: usize = rec.iter().map(|t| occurrences(t, b.exported_on).len()).sum();
    eprintln!("total occurrences (all recurring): {}", all_occ);
    eprintln!("transfers with refund flag: {}", b.transactions.iter().filter(|t| t.from != 0 && t.to != 0 && t.refund).count());
    eprintln!("negative pence rows: {}", b.transactions.iter().filter(|t| t.pence < 0).count());
    eprintln!(
        "cross-currency transfers: {} (with foreign: {})",
        b.transactions
            .iter()
            .filter(|t| t.from != 0 && t.to != 0 && b.accounts.iter().find(|a| a.id == t.from).map(|a| a.code.clone()) != b.accounts.iter().find(|a| a.id == t.to).map(|a| a.code.clone()))
            .count(),
        b.transactions.iter().filter(|t| t.from != 0 && t.to != 0 && t.foreign.is_some()).count()
    );
    let ends: std::collections::BTreeSet<String> = rec.iter().map(|t| t.repeat.as_ref().unwrap().end.clone()).collect();
    let units: std::collections::BTreeSet<String> = rec.iter().map(|t| t.repeat.as_ref().unwrap().unit.clone()).collect();
    let wks: std::collections::BTreeSet<String> = rec.iter().map(|t| t.repeat.as_ref().unwrap().weekend.clone()).collect();
    eprintln!("end values {:?} units {:?} weekend {:?}", ends, units, wks);
    // Are there non-recurring rows that look like materialised occurrences (same details+pence+from+to as a recurring row)?
    let mut looks_materialised = 0;
    for t in rec.iter() {
        looks_materialised += b.transactions.iter().filter(|o| o.repeat.is_none() && o.details == t.details && o.from == t.from && o.to == t.to && o.category == t.category).count();
    }
    eprintln!("non-recurring rows sharing details/accounts/category with a recurring row: {}", looks_materialised);
    // Zero-amount rows?
    eprintln!("rows with pence 0: {}", b.transactions.iter().filter(|t| t.pence == 0).count());
    eprintln!("rows dated after export: {}", b.transactions.iter().filter(|t| t.date > b.exported_on).count());
}
