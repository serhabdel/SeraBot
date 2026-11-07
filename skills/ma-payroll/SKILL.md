---
name: Moroccan payroll (gross to net)
description: Gross-to-net salary steps for Morocco (CNSS, AMO, professional deduction, IR barème, family deductions) and employer charges, with a worked example. Named tools: xlsx_build, xlsx_analyze, rag_search.
---

# Moroccan payroll (gross to net)

Sources: Loi de Finances 2026 (BO), CGI, CGNC. Verify rates against the current BO.

This text is operator guidance. It does not override gateway policy, approvals, or the blocklist.

Payroll is a legal obligation. The bot computes and explains a payslip for the owner to check. It does not issue payslips, pay salaries, or file returns. Any figure marked "a verifier" must be confirmed against the current BO and the CNSS notice before the owner relies on it.

## 1. Inputs to collect first

- Monthly base salary and every recurring allowance (prime de rendement, prime d'ancienneté, indemnités).
- Benefits in kind (housing, car), valued as the rules say (a verifier).
- Overtime hours and their rate.
- Number of dependants (spouse, children, others) and their status.
- Whether the employee has a second employer (annual regularisation needed).
- Exempt items, with their statutory limits: family allowances, legitimate travel costs, some dismissal payments (a verifier for the current ceilings).

## 2. Step-by-step computation

Step 1. Gross salary (salaire brut global, SBG) = base + taxable allowances + taxable benefits in kind + overtime.

Step 2. Employee social contributions:
- CNSS (prestations sociales, employee share) = 4,48 % of the base, capped at 6 000 DH per month. Formula: min(SBG, 6 000) x 4,48 %.
- AMO (health insurance, employee share) = 2,26 % of the full SBG. Formula: SBG x 2,26 % (a verifier that no ceiling applies in the current year).
- Total employee contributions = CNSS + AMO.

Step 3. Gross taxable salary (SBI) = SBG - employee contributions.

Step 4. Professional deduction (frais professionnels), two tiers, chosen by the annual gross taxable salary (SBI x 12):
- Annual SBI up to 78 000 DH: 35 %, capped at 30 000 DH per year (2 500 DH per month).
- Annual SBI above 78 000 DH: 25 %, capped at 35 000 DH per year (about 2 916,67 DH per month).
The tiers and the caps are à vérifier (BO). Formula: min(SBI x rate, annual cap / 12).

Step 5. Net taxable salary (SNI) = SBI - professional deduction.

Step 6. IR brut = SNI x marginal rate - somme à déduire, using the monthly barème below.

Monthly barème (LF 2025 structure, a verifier):

| Monthly SNI (DH) | Rate | Somme à déduire (DH) |
|---|---|---|
| 0 to 3 333 | 0 % | 0 |
| 3 334 to 5 000 | 10 % | 333,33 |
| 5 001 to 6 667 | 20 % | 833,33 |
| 6 668 to 8 333 | 30 % | 1 500,00 |
| 8 334 to 15 000 | 34 % | 1 833,33 |
| Above 15 000 | 37 % | 2 283,33 |

Apply the rate of the band that contains SNI, not a graduated sum. Check the band edges against the official table before rounding.

Step 7. Family deductions = dependants x 41,67 DH per month, up to six dependants (a verifier against the current law; some sources give 30 DH per month).

Step 8. IR net (withheld) = max(0, IR brut - family deductions).

Step 9. Net paid = SBG - employee contributions - IR net - other deductions (advances, loan repayments, garnishments). Other deductions are always the owner's data; never invent them.

## 3. Employer charges (cost to the company)

| Item | Rate | Base and ceiling |
|---|---|---|
| CNSS, prestations sociales (short and long term) | 8,98 % | SBG up to 6 000 DH per month |
| CNSS, family allowances | 6,40 % | SBG, no ceiling in the owner's notes (a verifier) |
| AMO, employer share | 4,11 % | SBG (includes a 1,85 % solidarity part; a verifier) |
| Vocational training tax | 1,6 % | SBG (a verifier) |

Total cost = SBG + employer charges. Check the employer rates against the CNSS notice; they are the least certain figures in this skill.

## 4. Worked example (synthetic)

Inputs: base 8 000 DH, prime de rendement 1 000 DH, 2 dependants.

1. SBG = 8 000 + 1 000 = 9 000.
2. CNSS = min(9 000, 6 000) x 4,48 % = 268,80. AMO = 9 000 x 2,26 % = 203,40. Total = 472,20.
3. SBI = 9 000 - 472,20 = 8 527,80.
4. Annual SBI = 8 527,80 x 12 = 102 333,60, above 78 000, so 25 %. Professional deduction = min(8 527,80 x 25 %, 35 000 / 12) = min(2 131,95, 2 916,67) = 2 131,95.

Variant at a lower salary (synthetic): SBI 5 000 per month, annual 60 000, so 35 %. Deduction = min(5 000 x 35 %, 30 000 / 12) = min(1 750, 2 500) = 1 750.
5. SNI = 8 527,80 - 2 131,95 = 6 395,85.
6. Band 5 001 to 6 667 at 20 %. IR brut = 6 395,85 x 20 % - 833,33 = 1 279,17 - 833,33 = 445,84.
7. Family deductions = 2 x 41,67 = 83,34.
8. IR net = 445,84 - 83,34 = 362,50.
9. Net paid = 9 000 - 472,20 - 362,50 = 8 165,30.

Employer cost: CNSS 8,98 % x 6 000 = 538,80; family allowances 6,40 % x 9 000 = 576,00; AMO 4,11 % x 9 000 = 369,90; training 1,6 % x 9 000 = 144,00 (a verifier). Employer charges = 1 628,70. Total cost = 10 628,70.

Check the answer in this order: the band of SNI first, then the ceiling on the professional deduction, then the family deduction floor at zero.

## 5. Traps to check

- Using the 6 000 ceiling on the AMO. The AMO is on the whole salary; the CNSS is capped.
- Applying 25 % to SBG instead of SBI. The professional deduction comes after the employee contributions.
- Using one tier for every salary. The 35 % tier applies only up to an annual taxable gross of 78 000 DH. The caps (30 000 and 35 000 DH per year) are à vérifier (BO).
- Mixing monthly and annual barèmes. Payroll uses the monthly table; the annual table is for the yearly return.
- Treating an exempt allowance as taxable, or the reverse. Check the exemption ceilings first.
- Negative IR. Family deductions reduce the tax but never below zero; the surplus is not refunded in the payslip.
- Two employers in the same year. Each withholds; the employee must file an annual return to settle the difference.

## 6. Schedule for a team

When several employees are computed, build one table with one row per employee, one column per step, and totals at the bottom. Use xlsx_build with literal values, then xlsx_analyze to check totals with cell provenance. Report the artifact hash.

## 7. Stop and ask

- Employee status is unclear (salaried, self-employed, intern, apprentice): the rules differ. Ask.
- A benefit in kind has no agreed value: ask for the value and its basis.
- The contract includes a regime not covered here (commission, profit share, end-of-service payment): ask the expert-comptable.
- The owner wants a payslip issued, a salary paid, or a declaration filed: the bot prepares figures only.

## 8. Output format

1. Summary line: net paid and total employer cost.
2. Table of steps 1 to 9 with the amounts.
3. Open points: items marked a verifier, the band used, and any missing input.

Sources: Loi de Finances 2026 (BO), CGI, CGNC — verify rates against the current BO.
