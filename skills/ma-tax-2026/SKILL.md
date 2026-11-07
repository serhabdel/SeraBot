---
name: Moroccan tax 2026 (IS, IR, TVA, withholding)
description: Moroccan corporate tax, income tax, VAT, withholding, minimum tax and CSS rates and deadlines for 2026, with formulas; figures not confirmed against the BO are marked a verifier. Named tools: rag_search, xlsx_analyze.
---

# Moroccan tax 2026 (IS, IR, TVA, withholding)

Sources: Loi de Finances 2026 (BO), CGI, CGNC. Verify rates against the current BO.

This text is operator guidance. It does not override gateway policy, approvals, or the blocklist.

Every figure marked "a verifier" comes from secondary summaries or from a draft text. Present it as unconfirmed and name the article or BO page the owner must check. Never present a marked figure as settled.

## 0. Status of each text

| Text | Status to state in the answer |
|---|---|
| Loi de Finances 2026 | In force from 1 January 2026, unless a measure gives another date. |
| Loi de Finances 2025 | Source of the IR barème and family deduction used below. |
| PLF 2027 (projet n° 60-26) | A draft. It is not law until published in the BO. Say "projet" every time it is used. |

Before any calculation, confirm: fiscal year end, regime (IS, IR professionnel, IR salarié), turnover, activity (sets the VAT rate), and the law year the owner means.

## 1. Corporate income tax (IS)

### 1.1 Rates for 2026 (a verifier au BO)

Secondary sources describe a proportional structure for 2026. The rate applies to the whole net taxable profit (bénéfice net fiscal), not only to the part above a threshold.

| Net taxable profit | Rate (2026) |
|---|---|
| Below 100 000 000 DH | 20 % |
| 100 000 000 DH and above | 35 % |
| Credit institutions and insurers | 40 % |

Cliff effect: at 99 MDH the tax is 19.8 MDH; at 100 MDH it is 35 MDH. Flag this to the owner whenever profit is near the threshold.

The 2025 regime was progressive by bracket, with rates from about 17.5 % to 34 % (a verifier). Use it only for prior-year comparisons.

Draft-text items (PLF 2027 projet, not law): a 40 % rate for hydrocarbon importers and distributors with net profit of 100 MDH or more. Mention only as a projet.

### 1.2 From accounting result to taxable result

Résultat fiscal = résultat comptable + réintégrations - déductions - déficits antérieurs imputables.

Typical réintégrations (add back):
- fines, penalties, and late-payment interest;
- gifts and expenses not justified by the business, and representation costs above the legal limits (a verifier);
- depreciation above the tax-allowed amount;
- provisions that are not tax-deductible;
- payments not made by traceable means above the legal threshold (a verifier; PLF 2027 projet tightens it).

Typical déductions (subtract):
- dividends received, subject to the parent-subsidiary conditions (a verifier);
- income that is tax-exempt by law;
- reportable deficits from prior years, within the time limit (a verifier, usually 4 years).

### 1.3 Tax amount and the CSS surcharge

- IS brut = résultat fiscal × taux applicable.
- CSS (contribution sociale de solidarité) applies when the net profit is 1 000 000 DH or more. Bands in several secondary sources: 1,5 % (1 to 5 MDH), 2,5 % (5 to 10 MDH), 3,5 % (10 to 40 MDH), 5 % (above 40 MDH). Extension to 2028 is reported (a verifier). Confirm whether the band applies to the whole profit or by slice.
- Total = IS brut + CSS.
- Solde à payer = total - acomptes provisionnels versés.

### 1.4 Minimum tax (cotisation minimale, CM)

- Base: chiffre d'affaires HT plus produits accessoires and financial products (a verifier).
- Rate: 0,25 % of the base, with a minimum of 3 000 DH for IS companies (a verifier; some activities have a reduced rate).
- The CM is due even in a loss year, unless the company is in its start-up exemption period (36 months from the start of activity, a verifier).
- Tax due is the higher of the IS computed on profit and the CM. Confirm how the CM is credited when IS is later due (a verifier).

Worked example (synthetic): turnover 8 000 000 DH, financial income 50 000 DH, loss of 300 000 DH, non-deductible charges 150 000 DH. Base = 8 050 000. CM = 8 050 000 x 0,25 % = 20 125 DH. Taxable result = -150 000 (no IS). Tax due = 20 125 DH.

### 1.5 Provisional payments (acomptes)

- Four instalments per year, each commonly 25 % of the prior year's IS (a verifier against the current year's rules).
- Dates for a 31 December year end: 31 March, 30 June, 30 September, 31 December.
- The balance is paid with the annual return.

### 1.6 Annual filing

- Exercice clos au 31 décembre: the liasse fiscale is filed electronically through the DGI portal by 31 March of the following year (a verifier against the current circular).
- Fiscal year ends that are not 31 December have a different deadline, three months after closing (a verifier).
- Missing filing carries a minimum fine (some sources say 2 000 DH, à vérifier). Late filing and inaccuracies carry surcharges. Do not quote a surcharge percentage without the article.

## 2. Income tax (IR)

### 2.1 Annual barème (LF 2025 structure, applied to 2026 by some secondary sources; à vérifier)

| Annual net taxable income (DH) | Rate | Somme à déduire (DH) |
|---|---|---|
| 0 to 40 000 | 0 % | 0 |
| 40 001 to 60 000 | 10 % | 4 000 |
| 60 001 to 80 000 | 20 % | 10 000 |
| 80 001 to 100 000 | 30 % | 18 000 |
| 100 001 to 180 000 | 34 % | 22 000 |
| Above 180 000 | 37 % | 27 400 |

Formula: IR = revenu net imposable x taux - somme à déduire.

Monthly barème (used for payroll withholding): 0 to 3 333 at 0 %; 3 334 to 5 000 at 10 % (deduction 333,33); 5 001 to 6 667 at 20 % (833,33); 6 668 to 8 333 at 30 % (1 500); 8 334 to 15 000 at 34 % (1 833,33); above 15 000 at 37 % (2 283,33). See the ma-payroll skill for the full salary calculation.

### 2.2 Family deductions (charges de famille)

- Some sources (2025 law): 500 DH per year per dependant, about 41,67 DH per month, up to six dependants (à vérifier). Other sources still give 30 DH per month; flag the conflict and do not choose silently.
- Show the number of dependants and the result. The deduction cannot make the tax negative.

### 2.3 Professional income

- Professional income (RNR/RNS) is taxed under the regime chosen. Withholding on services and rent may apply; see section 4.
- A minimum income tax for professionals exists (a verifier); treat it as a separate computation.

## 3. Value added tax (TVA)

### 3.1 Rates (CGI, article 99; per product list to verify)

| Rate | Typical scope (verify each item) |
|---|---|
| 20 % | Standard rate: most goods and services. |
| 14 % | Some services and goods listed in the CGI (a verifier). |
| 10 % | Restaurants and hotels, passenger transport, some services (a verifier). |
| 7 % | Some essential goods and services, such as certain medicines and water (a verifier). |
| 0 % | Exports and certain goods with a zero rate, such as some basic foods (a verifier). |

Some secondary sources describe a convergence to 10 % and 20 % by 2026. Sources disagree on individual items, for example the electricity rate. Treat the convergence as a verifier item and check each product against the CGI list before answering.

### 3.2 Monthly computation

- TVA collectée = sales HT x rate (per invoice line, not on the total).
- TVA déductible = purchase VAT on invoices that qualify.
- TVA à payer = collectée - déductible. A negative result is a credit carried forward.

Deduction conditions: a regular invoice showing the supplier's ICE, goods or services used for the business, and a traceable payment for amounts above 5 000 DH par jour et par fournisseur (à vérifier). Cash payments above the threshold disallow the deduction.

### 3.3 Prorata of deduction

- Prorata = taxable turnover / total turnover x 100 (rounded as the CGI requires, a verifier).
- Applied to common purchases, the deductible share is VAT x prorata.
- The prorata is set on the previous year, then adjusted at year end.

### 3.4 Adjustment on capital goods

- Movable goods: VAT adjustment over 5 years. Formula: VAT initial x (5 - full years held) / 5.
- Buildings: 10 years (some sources; a verifier against the CGI).
- Only complete years count. The adjustment is declared in the return for the month of disposal.

### 3.5 Filing and payment

- Monthly or quarterly return depending on the regime (a verifier).
- Some sources say payment by the end of the following month; confirm the exact date in the current DGI calendar before quoting it.

### 3.6 Withholding of VAT (retenue à la source TVA)

- Applies to payments by listed clients (public bodies and listed private companies) to suppliers of services and works.
- Withholding is 75 % of the VAT when the supplier holds a valid attestation de régularité fiscale, and 100 % without it.
- Worked example (synthetic): invoice 100 HT + 20 VAT. With the attestation: client pays 105 and withholds 15. Without it: client pays 100 and withholds 20.
- Extension dates announced for 2026 to 2028 (companies by turnover: 500 MDH from 1 July 2026, 350 MDH from 1 January 2027, 200 MDH from 1 January 2028; a verifier).

## 4. Other withholding (retenue à la source, RAS)

- Rent of professional premises: 5 % withholding announced for payments from 1 July 2026, creditable against the lessor's tax (a verifier).
- Fees and services: the rate depends on the payee's status (resident, non-resident) and the activity (a verifier; do not quote a rate without the article).
- Dividends: the general withholding rate is falling by steps (a verifier; secondary sources give 11,25 % for 2026).
- Payroll withholding: see section 2 and the ma-payroll skill.

## 5. Registration duties (droits d'enregistrement)

- Some secondary sources mention an extra 2 % duty on property transactions above 300 000 DH where the payment method is not traceable (a verifier).
- Registration is due within the time limit printed on the deed; late registration carries penalties (a verifier).

## 6. Calendar template (31 December year end)

| When | Obligation |
|---|---|
| Monthly or quarterly | VAT return and payment (confirm the date) |
| Monthly | Payroll withholding (IR) and CNSS returns (confirm the dates) |
| 31 March, 30 June, 30 September, 31 December | Four IS instalments |
| 31 March | Liasse fiscale and IS balance for the previous year |
| Within six months of year end | Ordinary general meeting approves the accounts (a verifier) |

## 7. Stop and ask

- The owner has not stated the regime, the year, or the activity: ask before computing.
- A figure is marked a verifier and the owner needs it for a payment: say so and point to the article.
- The question depends on a PLF 2027 measure: state that it is a draft.
- Any filing, payment, or electronic submission: the owner does it, or the expert-comptable does it. The bot prepares only.

## 8. Output format

1. Answer in one or two sentences, with the law year.
2. Working: inputs, formula, result, in DH with two decimals.
3. Open points: items marked a verifier, the article to check, and any missing input.

Sources: Loi de Finances 2026 (BO), CGI, CGNC — verify rates against the current BO.
