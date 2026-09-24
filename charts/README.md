# Chart templates

A chart template is a starting income and expense side of an entity's chart of accounts (see [accounting model](../docs/data-model.md)). A full chart template also lists the balance-sheet accounts (T7). Applying one creates accounts in that entity; nothing is shared between entities afterwards. Codes are what make two entities comparable: an account with code 6100 means the same thing in every entity built from the same template.

Spending categories are expense accounts. Templates define the income and expense accounts used to classify transactions.

| File | Audience |
|---|---|
| `personal-nomad.json` | one person living across countries, several currencies, remote income |
| `personal-family.json` | a household with children: two earners, a home, a car, school and childcare |
| `us-llc-single-member.json` | full chart: a freelancer or consultant billing through a US single-member LLC (Schedule C), with the bank, receivables, payables and the member's equity and balances |
| `br-mei-servicos.json` | a Brazilian MEI providing services |
| `br-empresa-servicos.json` | a small Brazilian services company in the Simples Nacional (ITG 1000 model chart) |
| `company-services.json` | generic services-company operating accounts; examples include a Singapore Pte. Ltd., US C-corporation, and Estonian OÜ |
| `sg-pte-ltd.json` | full chart: a Singapore private company limited by shares, with SFRS-style statement lines, GST and group balances |
| `es-sl-pgc-pymes.json` | full chart: a Spanish sociedad limitada on the Plan General de Contabilidad de PYMES |
| `pt-individual.json` | full chart: an individual resident in Portugal who holds shares in, and lends to, foreign companies |
| `remap/` | per-entity remap plans: a target chart in the same account shape plus `moves` from existing category names to paths. A template is the generic starting point; a remap plan is one entity's concrete target |

## 1. File shape

```json
{
  "id": "personal-nomad",
  "name": "Personal · nomad / expat",
  "for": "one person living across countries, several currencies, remote income",
  "accounts": [
    { "code": "4000", "path": "Income", "type": "income", "placeholder": true, "description": "" },
    { "code": "6110", "path": "Expenses:Housing:Rent", "type": "expense", "placeholder": false, "description": "Long-term rent" },
    { "code": "2130", "path": "Liabilities:Current liabilities:GST output tax payable", "type": "liability", "placeholder": false,
      "description": "", "statutory_code": "sfrs:TradeAndOtherPayables", "tax_treatment": "output_tax", "intercompany": false }
  ]
}
```

| Field | Meaning |
|---|---|
| `id` | File name without `.json`. Kebab-case, audience prefix first: `personal-`, `us-`, `br-`. |
| `name` | Shown in the picker. `Audience · variant`. |
| `for` | One line naming who the template fits. |
| `path` | Segments joined by `:`. The first segment is the account type's root name; the rest are account names. |
| `code` | Four digits, the account's number. Becomes the account's `code` in the engine. |
| `type` | `asset`, `liability`, `equity`, `income` or `expense`, decided by the first segment. |
| `placeholder` | `true` for accounts that hold children and never postings. |
| `description` | English. Goes to the account's `notes`. For Portuguese or Spanish account names, the description is the English gloss. Figures quoted in descriptions (rates, thresholds) carry their year and are guidance, not advice. |
| `statutory_code` | Optional string. The code of the matching line in a national chart or reporting taxonomy, such as a PGC account number (`572`) or a statement line (`sfrs:TradeAndOtherReceivables`). Free text. Goes to the account's `statutory_code`. |
| `tax_treatment` | Optional string. A free-text tag for tax reports, such as `output_tax`, `input_tax`, `withholding_payable`, `income_tax`, `intercompany`, `owner_capital`, `owner_distribution` or `foreign_capital_income`. Goes to the account's `tax_treatment`. |
| `intercompany` | Optional boolean. `true` when the account holds balances or flows with a related party: a parent, a subsidiary, a member or a shareholder. Goes to the account's `intercompany` flag. |

A node can also carry the optional account identifiers `country`, `iban`, `bic`, `institution` and `location` as strings. They go to the columns of the same name. A node without them leaves the values of the account unchanged. The bundled templates do not use them.

The three statutory keys are optional, so a template without them stays valid. Statutory codes in the bundled templates are indicative: they show where a line usually goes, not a filing position. `means accounts --entity <id> --json` lists each account with its path, type, subtype, commodity, statutory keys and balance.

Rules a template MUST satisfy. `validate_charts.py` (section 5) checks each one.

- **T1** The first segment is `Income` or `Expenses`, the engine's fixed root names (`AccountType::root_name`). A full chart (T7) also uses `Assets`, `Liabilities` and `Equity`. Portuguese and Spanish templates keep the English roots and name their children in their own language.
- **T2** Every parent used by a path is itself listed, before its children, with `placeholder: true`. A placeholder has at least one child; a postable account has none.
- **T3** Paths and codes are unique within the template. Depth is at most three segments.
- **T4** An income and expense template has no assets, liabilities or equity. Bank accounts, cards and holdings come from onboarding and imports; `Opening balances`, `Uncategorized` and `FX gain/loss` are created with the entity (`create_system_accounts`). A template MUST NOT add its own uncategorised or FX accounts. To give a system account a code, list it at its system path (T7).
- **T5** Between 45 and 80 accounts. A full chart (T7) holds up to 90.
- **T6** Equity flows stay out of the income and expense side: owner draws, contributions, distributions paid, loan principal, refundable deposits, taxes withheld but not yet paid. A full chart books them on its balance-sheet accounts.
- **T7** A full chart lists balance-sheet accounts under `Assets`, `Liabilities` and `Equity`, for an entity that reports a statement of financial position. It lists a system account at its system path: `Equity:Owner contributions`, `Equity:Owner distributions`, `Equity:Opening balances`, `Expenses:FX gain/loss`. The apply then codes and tags the system account and makes no twin. A full chart MUST NOT turn a system account into a placeholder, because transfers between entities post to it.

## 2. Numbering

One scheme for every template, so a report that groups by code reads the same across entities.

| Range | Holds |
|---|---|
| 1000 | `Assets` root in a full chart; groups on hundreds from `1100` |
| 2000 | `Liabilities` root in a full chart; groups on hundreds from `2100` |
| 3000 | `Equity` root in a full chart; groups on hundreds from `3100` |
| 4000 | `Income` root |
| 4100–4899 | income groups, one per hundred |
| 4900–4999 | deductions from gross revenue in the Brazilian templates (deduções da receita: DAS, ISS withheld, cancellations, unconditional discounts). Contra-revenue, debit balance |
| 5000–5999 | reserved for cost of sales if a template ever needs a gross margin line |
| 6000 | `Expenses` root |
| 6100–7899 | expense groups, one per hundred |
| 7900 | `Expenses:Miscellaneous` when a template keeps it as a single postable account |

- **N1** Roots are `1000`, `2000`, `3000`, `4000` and `6000`. A group (second segment) is a round hundred: `6100`, `6200`. A postable account (third segment) takes a ten inside its parent's hundred: `6110` to `6190`. A postable account directly under a root, such as `Expenses:Miscellaneous`, takes a round hundred like a group.
- **N2** Room to insert: the fives (`6115`) and any unused ten. A group holds at most nine postable accounts; a tenth means the group splits.
- **N3** Codes ascend in file order, parents before children.
- **N4** A code means the same thing in every entity built from the same template. Across templates, only the fixed slots in the table above align (the roots, `4900`, `7900`). The `statutory_code` compares two entities under the same national chart. A Brazilian company and a US company compare by account name and type.
- **N5** Taxes. The personal templates give each country its own group: `Taxes Portugal`, `Taxes Brazil`, `Taxes other countries`. The year's total per jurisdiction is then one balance, and a tax that is one line on a return is one account. The business templates keep one tax group, because the entity has one jurisdiction.

## 3. Applying a template to an entity

- **A1** For each account, parents before children, find or create the account by path in the entity (`accounts::ensure_account` creates missing parents as placeholders). Set `type` and `code`, and `description` into `notes` when the account has no notes. `placeholder: true` makes an account a placeholder only when it has no postings. All accounts take the entity's functional currency. A postable asset gets the subtype `bank` and a postable liability `card`; change the subtype after applying where it does not fit.
- **A2** Applying is idempotent: a path that already exists keeps its name, type and children, and its `code` is filled only if empty.
- **A3** A user edits after applying: delete unused postable accounts, rename `Salary A` to the person, add a country subtree. New accounts follow N1 and N2 so the entity stays comparable.
- **A4** The engine refuses postings on placeholders (`journal.rs`), so every group listed as a placeholder must keep at least one postable child or be removed.
- **A5** Bundled payments split by entry template, not by account. The MEI's DAS is one bank line and two or three postings: `DAS-MEI – INSS`, `DAS-MEI – ISS`, `DAS-MEI – ICMS`. An entry template with one line per component drafts it (see [templates](../docs/data-model.md#payees-tags-templates-rules-and-budgets)).
- **A6** Each apply writes the statutory keys that a node gives. A re-apply thus updates `statutory_code`, `tax_treatment` and `intercompany` on existing accounts. A node without a key leaves that value of the account unchanged. An empty string or `false` clears it. These keys do not touch postings, so balances do not change.

## 4. Templates

### 4.1 `personal-nomad` · Personal · nomad / expat

79 accounts. Groups follow the COICOP 2018 divisions: food, housing, health, transport, information and communication, recreation, insurance and financial services, personal care. COICOP is the international standard for classifying household spending. On top come the nomad's own groups: `Housing:Short-term stays` apart from `Transport and travel:Accommodation on trips`, `Immigration and documents`, one tax subtree per country, `Family and giving:Support to parents`. `Bank and FX fees` splits into account fees, conversion fees, transfer fees and interest.

Judgment calls:

- Portugal's income tax is named `IRS` (Imposto sobre o Rendimento das Pessoas Singulares), spelled out in the description.
- `IOF` sits under `Taxes Brazil`, one balance for the year's IOF. In the two Brazilian business templates IOF sits under financial expenses, where ITG 1000 puts it.
- Exchange-rate differences are not an account here: the engine posts them to the system `FX gain/loss` account. Only explicit conversion fees are.
- `Income:Work:Owner distributions received` mirrors the LLC template's equity side of the same transfer (see [transfers between entities](../docs/data-model.md#journal-entries-and-postings)).

Sources:

- COICOP 2018, UN Statistics Division: https://unstats.un.org/unsd/classifications/unsdclassifications/COICOP_2018_Pre-edited_white-cover_version_2018-12-26.pdf (divisions 01–13 and their groups); overview at https://unstats.un.org/unsd/classifications/coicop
- GnuCash "Common Accounts" hierarchy (income and expense defaults): https://raw.githubusercontent.com/Gnucash/gnucash/stable/data/accounts/C/acctchrt_common.gnucash-xea; GnuCash Guide, Accounts chapter: https://www.gnucash.org/docs/v5/C/gnucash-guide/chapter_accts.html
- Portugal, personal income tax and the NHR regime and its successor IFICI (20% flat rate on eligible income, ten years; NHR closed to new applicants from 2024): https://taxsummaries.pwc.com/portugal/individual/taxes-on-personal-income and https://taxsummaries.pwc.com/portugal/individual/other-tax-credits-and-incentives (PwC Worldwide Tax Summaries)
- Portugal, social security for self-employed workers (21.4%, base of one third of the relevant remuneration): https://taxsummaries.pwc.com/portugal/individual/other-taxes
- Brazil, carnê-leão (monthly income tax on income from individuals or from abroad, due the last business day of the following month): https://www.gov.br/receitafederal/pt-br/assuntos/meu-imposto-de-renda/pagamento/carne-leao
- Brazil, INSS plans for contribuinte individual and facultativo (20% code 1007, 11% code 1163, due the 15th of the following month): https://www.gov.br/inss/pt-br/direitos-e-deveres/inscricao-e-contribuicao/contribuicao-dos-segurados-facultativo-e-contribuinte-individual

### 4.2 `personal-family` · Personal · family

76 accounts. COICOP divisions again, plus GnuCash's `Auto`, `Insurance`, `Utilities` and `Taxes` groups. A `Children` group collects what exists because of the kids: childcare, school, activities, toys, clothing and baby supplies, pocket money. The kids' share of food, housing and holidays stays in those groups, which is how household surveys classify it.

Judgment calls:

- Groceries and dining out are separate accounts because every household survey separates food at home from food away from home (COICOP 01 versus 11.1).
- `Mortgage interest` and `Car loan interest and leasing` hold only the interest; the principal reduces the liability (T6).
- `Salary A` and `Salary B` are placeholders for names, not a claim about who earns what.
- `Expenses:Miscellaneous` is a single postable account at `7900` (N1); it is not the system `Uncategorized` account, which holds statement lines still to be classified.

Sources:

- COICOP 2018 (as above), in particular divisions 04, 05, 06, 07, 09.3 (pets), 10 (education), 13.3 (child care as social protection).
- GnuCash "Common Accounts" and "Childcare Expenses" templates: https://raw.githubusercontent.com/Gnucash/gnucash/stable/data/accounts/C/acctchrt_common.gnucash-xea and https://raw.githubusercontent.com/Gnucash/gnucash/stable/data/accounts/C/acctchrt_childcare.gnucash-xea

### 4.3 `us-llc-single-member` · Business · US single-member LLC

86 accounts: 75 income and expense accounts and 11 balance-sheet accounts for the bank, the open invoices and the member. A single-member LLC that has not elected corporate treatment is a disregarded entity: its income and expenses land on the owner's Schedule C. Every expense group names the Schedule C line it feeds. The income side mirrors Part I in order: gross receipts (line 1), returns and allowances (line 2), other income (line 6).

Judgment calls:

- `Income:Returns and allowances` is a contra-revenue group at `4200`, not in the `4900` slot. Schedule C reads gross receipts, then returns, then other income, and the `4900` slot is for the Brazilian deduções da receita.
- Owner draws and contributions are equity. They are not on the income and expense side (T6). The chart lists them at the system paths `Equity:Owner contributions` (member capital contributions, `owner_capital`) and `Equity:Owner distributions` (member distributions, `owner_distribution`), so the apply tags the system accounts (T7).
- `Liabilities:Due to member/parent` holds business costs that the member or a parent paid; `Assets:Due from member/parent` holds personal costs that the LLC paid. Both are `intercompany`. Their `tax_treatment` names the equity account each usually closes to: `owner_capital` and `owner_distribution`.
- A single-member LLC still has a bank account and open items. `Assets:Bank` is a group with one `Operating account`, and onboarding and imports add each bank under it. `Assets:Accounts receivable` and `Liabilities:Accounts payable` are the control accounts of sales and purchase invoices.
- Income leaves carry the Schedule C line as `statutory_code` (`schc:1`, `schc:2`, `schc:6`). Expense groups name their line in the description, because some lines vary by practice.
- Federal income tax and self-employment tax are the owner's, not deductible on Schedule C, and have no account.
- `Business meals (50% deductible)` carries the limit in its name. The full cost is booked; the return applies the 50%.
- The owner's health insurance is a Schedule 1 deduction and is absent from `Insurance`.
- `Equipment` accounts are expenses, with the $2,500 de minimis safe harbor in the description; assets above it are capitalised outside this chart.
- Payment processing fees are marked "line 10 or 27a" because practice varies; the account exists either way.

Sources:

- Instructions for Schedule C (Form 1040), 2025, lines 1–32: https://www.irs.gov/instructions/i1040sc; form page: https://www.irs.gov/forms-pubs/about-schedule-c-form-1040
- Single-member LLCs as disregarded entities: https://www.irs.gov/businesses/small-businesses-self-employed/single-member-limited-liability-companies
- Publication 463 (2025), travel, gift and car expenses: 50% meals, $25 gifts, 70 cents per mile: https://www.irs.gov/publications/p463
- Publication 334 (2025), Tax Guide for Small Business (meals limit, interest, business use of home): https://www.irs.gov/publications/p334
- Tangible property regulations, de minimis safe harbor of $2,500 per item or invoice: https://www.irs.gov/businesses/small-businesses-self-employed/tangible-property-final-regulations
- Topic 513, work-related education for the self-employed: https://www.irs.gov/taxtopics/tc513
- Delaware annual LLC tax, $300 due June 1, no annual report: https://corp.delaware.gov/?p=188
- California annual LLC tax, $800, Form 3522: https://www.ftb.ca.gov/file/business/types/limited-liability-company/index.html

### 4.4 `br-mei-servicos` · Negócio · MEI de serviços

74 accounts. The MEI pays a fixed monthly DAS: INSS at 5% of the minimum wage, ISS R$ 5,00 for services, ICMS R$ 1,00 for goods. It reports gross revenue monthly, split between revenue with a fiscal document and revenue without one. The chart follows both. Three DAS component accounts sit under `Tributos e contribuições`; `Receita de serviços` splits into `com nota fiscal`, `sem nota fiscal` and `para o exterior`.

Judgment calls:

- The DAS is an expense here: a fixed amount whose INSS part is the owner's own social security. In the company template the DAS is a deduction from revenue (4.5).
- `INSS complementar` (the optional 15% top-up, GPS code 1910) is included because it is usually paid from the business account, with a description saying it is the owner's choice.
- The MEI keeps no depreciation schedule, so `Equipamentos e materiais` holds purchases as expenses.
- A `Pessoal` group exists because the MEI may employ one person (two under the 2026 law), with the MEI's reduced 3% employer INSS in the description.
- Retiradas (owner withdrawals) are equity and absent (T6).

Sources:

- Lei Complementar 123/2006, art. 18-A (MEI, R$ 81.000 ceiling, fixed monthly amounts), art. 18-C (one employee, 3% employer contribution), art. 13 (taxes in the Simples): https://www.planalto.gov.br/ccivil_03/leis/lcp/lcp123.htm (consolidated text also at https://www2.camara.leg.br/legin/fed/leicom/2006/leicomplementar-123-14-dezembro-2006-548099-normaatualizada-pl.html)
- Receita Federal, DAS-MEI values for 2026 (R$ 81,05 INSS on the R$ 1.621 minimum wage, R$ 5,00 ISS, R$ 1,00 ICMS), notice of 02/01/2026: https://www8.receita.fazenda.gov.br/simplesnacional/noticias/NoticiaCompleta.aspx?id=c3b2044c-ff97-432a-b33c-ecf2a3df6dc3
- Teto do MEI (R$ 81.000; R$ 110.000 in 2027 and R$ 140.000 in 2028; up to two employees), gov.br, 29/06/2026: https://www.gov.br/memp/pt-br/teto-do-mei
- Relatório Mensal de Receitas Brutas (due the 20th of the following month, revenue with and without a fiscal document): https://www.gov.br/pt-br/servicos/baixar-relatorio-mensal-de-receitas-brutas and the form at https://www.gov.br/empresas-e-negocios/pt-br/empreendedor/servicos-para-mei/declaracao-anual-de-faturamento/relatorio_mensal_das_receitas_brutas.doc/view
- INSS complement for the MEI (15%, GPS code 1910): https://fenacon.org.br/noticias/veja-como-elevar-em-15-o-valor-da-aposentadoria-de-quem-e-mei/; INSS contribution plans: https://www.gov.br/inss/pt-br/direitos-e-deveres/inscricao-e-contribuicao/contribuicao-dos-segurados-facultativo-e-contribuinte-individual

### 4.5 `br-empresa-servicos` · Negócio · empresa de serviços (Simples Nacional)

78 accounts. The reference is the CFC's ITG 1000 model chart for microentities and small companies. Its income side is revenue (`Serviços Prestados`), deductions, financial income and other income. The deductions group is `Deduções de Tributos, Abatimentos e Devoluções`; the 2022 revision names `Simples Nacional` and `ISS` inside it. Its expense side is personnel, administrative, selling, financial and other expenses. Group names here are the ITG 1000 names. Codes follow section 2 rather than ITG 1000's `3.1`/`3.2` numbering, so the template stays comparable with the others.

Judgment calls:

- The DAS is a deduction from revenue (`Income:Deduções da receita bruta:Simples Nacional (DAS)`). That is how ITG 1000's 2022 chart lists it and how most Simples books are kept. Strictly, the IRPJ and CSLL share belongs after operating profit; an entry template can split it.
- `ISS retido na fonte` is its own deduction because clients withhold it and it reduces the ISS share of the DAS.
- Direct costs are not a separate `Custos` group. ITG 1000 has one (`3.2.1 Custos dos Serviços Prestados`); the `Serviços de terceiros` description says where to move accounts if a gross margin line is wanted.
- `Pró-labore` is an expense; the partners' profit distributions are equity and absent (T6).
- Employer INSS is inside the DAS for Anexo III and V activities and paid separately only for Anexo IV; `INSS e FGTS` and `Autônomos (RPA)` say so.
- `Equipamentos de pequeno valor` uses the RIR/2018 art. 313 threshold (R$ 1.200 or useful life under a year); anything above it is capitalised and reaches this chart through `Depreciação e amortização`.

Sources:

- ITG 1000, Resolução CFC 1.418/12, Anexo 1 (plano de contas simplificado) and Anexo 3 (demonstração do resultado): https://crcgo.org.br/wp-content/uploads/2022/12/ResolucaoCFC_-1418.pdf
- ITG 1000 revised 15/12/2022, Anexo VII (plano de contas para pequena empresa: deductions PIS, COFINS, ISS, ICMS, Simples Nacional; personnel including pró-labore; administrative; financial including IOF): https://www.legisweb.com.br/legislacao/?id=440233; CFC consultation page: https://www.gov.br/participamaisbrasil/itg-1000-normas-aplicaveis-e-modelos-de-plano-de-contas-e-demonstracoes-contabeis-para-microentidade-e-pequena-empresa
- Lei Complementar 123/2006, art. 3º (ME up to R$ 360.000, EPP up to R$ 4.800.000), art. 13 (IRPJ, IPI, CSLL, COFINS, PIS/Pasep, CPP, ICMS, ISS in one document), art. 21 §4º (ISS withheld at source): links as in 4.4
- Resolução CGSN 140/2018, art. 4º (receita bruta), art. 25 (Anexos III, IV, V and the 28% fator r), art. 27 (ISS retido): http://normas.receita.fazenda.gov.br/sijut2consulta/link.action?idAto=92278; Simples Nacional overview: https://www8.receita.fazenda.gov.br/SimplesNacional/Documentos/Pagina.aspx?id=3
- RIR/2018 (Decreto 9.580/2018) art. 313, R$ 1.200 or one year of useful life: https://www.planalto.gov.br/ccivil_03/_ato2015-2018/2018/decreto/D9580.htm

### 4.6 `sg-pte-ltd` · Business · Singapore private limited company

77 accounts, full chart. The groups follow the statement of financial position and the statement of comprehensive income of a small company under the Singapore Financial Reporting Standards (SFRS, or SFRS for Small Entities). Each leaf names its statement line in `statutory_code`, such as `sfrs:TradeAndOtherReceivables` or `sfrs:CostOfSales`. These names are indicative labels for the line, not an official taxonomy code.

Judgment calls:

- `GST input tax receivable` is `input_tax` and `GST output tax payable` is `output_tax`. Only a GST-registered company uses them. GST that the company cannot claim goes to `Non-claimable GST` (`blocked_input_tax`).
- Group balances are `intercompany`: the investment in and dividends from a subsidiary, amounts due to and from related parties, the shareholder loan and its interest, services to and management fees from group companies. A consolidation or a transfer pricing review filters on the flag.
- `Owner contributions` and `Owner distributions` are the system accounts (T7). Move contributions to `Share capital` when shares are allotted; close distributions against `Retained earnings`.
- Exchange differences go to the system `FX gain/loss` account, which the template codes as `7800` (T7).
- `Income tax payable` and `Income tax expense` are `income_tax`.

Sources:

- Accounting Standards Council, SFRS and SFRS for Small Entities: https://www.asc.gov.sg
- IRAS, GST registration and input tax claims: https://www.iras.gov.sg/taxes/goods-services-tax-(gst)
- ACRA, annual filing and the small company audit exemption: https://www.acra.gov.sg

### 4.7 `es-sl-pgc-pymes` · Negocio · sociedad limitada (PGC PYMES)

81 accounts, full chart. Account names are the Spanish names of the Plan General de Contabilidad de PYMES (Real Decreto 1515/2007). The description is the English gloss. Each leaf carries its PGC account number as `statutory_code`, for example `572` Bancos, `4750` HP acreedora por IVA, `705` Prestaciones de servicios. The `code` follows section 2 so the template stays comparable with the others.

Judgment calls:

- VAT: `472` IVA soportado is `input_tax`, `477` IVA repercutido is `output_tax`, `4750` is `vat_payable` and `4700` is `vat_receivable`. Withholding is `withholding_payable` (`4751`) or `withholding_receivable` (`473`). Corporate income tax is `income_tax` (`4752` and `630`).
- `551` Cuenta corriente con socios sits under assets and is `intercompany`. A credit balance is a debt of the company to the partner.
- The engine posts exchange differences to the system `FX gain/loss` account. The PGC splits them into `668` and `768`. Reclassify the year's losses and gains to those accounts before the annual accounts.
- `Owner contributions` carries `118` Aportaciones de socios and `Owner distributions` carries `557` Dividendo activo a cuenta. Both keep their system names (T7).

Sources:

- Real Decreto 1515/2007, Plan General de Contabilidad de Pequeñas y Medianas Empresas, cuadro de cuentas (fourth part), published in the Boletín Oficial del Estado of 21 November 2007: https://www.boe.es

### 4.8 `pt-individual` · Personal · Portugal resident with foreign shareholdings

80 accounts, full chart. It fits an individual who is tax resident in Portugal and holds shares in companies abroad. Assets hold domestic and foreign bank accounts, one account per shareholding (`Shares in Company A`) and loans to those companies. Income follows the IRS income categories: `statutory_code` is `irs:A` (employment), `irs:B` (business and professional), `irs:E` (capital income) or `irs:G` (capital gains). The codes are indicative.

Judgment calls:

- Dividends have one account per source country (`Dividends from Country A`). Dividend and foreign interest accounts are `foreign_capital_income`. Domestic bank interest is `domestic_capital_income`, because tax is usually withheld at source.
- Foreign tax withheld is an expense under `Taxes other countries` and is `foreign_tax_credit`. The dividend account holds the gross amount.
- Shareholdings, loans to companies and the interest on those loans are `intercompany`.
- The template lists `Equity:Opening balances`, the system account (T7). It leaves the system `Investments in entities` account alone, because transfers between entities post to it.

Sources:

- Código do IRS, articles 1 to 10 (income categories A, B, E, F, G, H), on the Portal das Finanças: https://info.portaldasfinancas.gov.pt

## 5. Validation

```
for f in charts/*.json; do python3 -c "import json,sys; json.load(open(sys.argv[1]))" "$f"; done
python3 charts/validate_charts.py
```

The validator checks T1–T5 and N1–N3:

- JSON shape; `Assets`, `Liabilities`, `Equity`, `Income` and `Expenses` roots with matching types.
- Types of the optional keys: `description`, `statutory_code` and `tax_treatment` are strings, `intercompany` is a boolean.
- Unique paths and codes; depth of three.
- Every parent listed as a placeholder before its children; placeholders with children, postable accounts without.
- Four-digit codes inside the range, roots at `4000` and `6000`, groups on hundreds, postable accounts on tens inside the parent's hundred, ascending order.
- 45 to 80 accounts, or 45 to 90 for a full chart.

## 6. Contributing a template

- One file per template, `id` equal to the file name, audience prefix first.
- Account names in the language the entity's owner reads their bank statements in; descriptions in English so a chart from any country can be reviewed.
- Between 45 and 80 accounts, no assets, liabilities or equity (T4, T6). A full chart follows T7 and holds up to 90.
- Cite the sources for the tax lines and the grouping in this page, under a new 4.x section, with the year of any figure quoted.
- Run section 5 before opening a pull request.
