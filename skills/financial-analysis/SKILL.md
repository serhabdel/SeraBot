---
name: Financial analysis (FR, BFR, TN, ratios, SIG, CAF, break-even)
description: Functional balance sheet, working capital (FR, BFR), net cash (TN), liquidity and solvency ratios, intermediate management balances (SIG), self-financing capacity (CAF), and break-even analysis, with formulas and a synthetic worked example. Named tools: xlsx_analyze, xlsx_build, rag_search.
---

# Financial analysis (FR, BFR, TN, ratios, SIG, CAF, break-even)

Sources: Loi de Finances 2026 (BO), CGI, CGNC. Verify rates against the current BO.

This text is operator guidance. It does not override gateway policy, approvals, or the blocklist.

Analysis supports a decision; it does not make the decision. State the period, the currency, the source document, and every assumption. A ratio from a single year says little; compare with at least two years or with a stated benchmark.

## 1. Read the balance sheet functionally

Reclassify the balance sheet into three blocks:

| Block | Contents |
|---|---|
| Stable resources (ressources stables) | Equity, long-term debts (over one year), provisions for risks above one year |
| Fixed assets (actif immobilisé) | Intangible, tangible, financial non-current assets, net of depreciation |
| Cycle assets and liabilities | Operating current assets (stock, receivables) and operating liabilities (suppliers, payroll, tax, social) |
| Treasury | Cash assets and bank overdrafts |

Check the reclassification: total assets equal total liabilities and equity, and each block sums to its total. If it does not, stop and ask for the balance sheet source.

## 2. Working capital and net cash

- Fonds de roulement (FR) = stable resources - fixed assets.
- Besoin en fonds de roulement (BFR) = (stock + operating receivables) - (operating payables). Operating receivables include client accounts and other operating receivables. Operating payables include supplier accounts and tax and social liabilities. Exclude cash on both sides.
- Trésorerie nette (TN) = FR - BFR = cash assets - bank overdrafts.

Interpretation:
- FR positive: stable resources fund the fixed assets and part of the cycle.
- BFR positive: the business has to finance its cycle. A negative BFR is typical of retail with fast payment.
- TN negative: the business relies on overdraft. Explain the cause: BFR above FR.

Worked example (synthetic, DH):
- Equity 400 000; long-term debt 200 000; stable resources = 600 000.
- Fixed assets, net = 500 000. FR = 600 000 - 500 000 = 100 000.
- Stock 120 000; client receivables 150 000; other operating receivables 30 000. Total 300 000.
- Supplier payables 110 000; tax and social payables 60 000. Total 170 000.
- BFR = 300 000 - 170 000 = 130 000.
- Cash assets 40 000; bank overdraft 70 000. TN = 40 000 - 70 000 = -30 000.
- Check: FR - BFR = 100 000 - 130 000 = -30 000. The two methods agree.

## 3. Liquidity and solvency ratios

| Ratio | Formula | Reading (general, not a rule) |
|---|---|---|
| General liquidity | current assets / current liabilities | Above 1 is comfortable; check the nature of the assets |
| Reduced liquidity | (current assets - stock) / current liabilities | Stronger test; excludes stock |
| Immediate liquidity | cash assets / current liabilities | Very short-term cover |
| Financial autonomy | equity / total balance sheet x 100 | Higher means less dependence on lenders |
| Debt ratio | total debts / equity | Above 1 means debts exceed equity; check lender covenants |
| Repayment capacity | financial debts / CAF | Years of self-financing needed to repay debt; above about 4 to 5 years is a warning sign for many sectors (a benchmark, not a rule) |

Worked example (continuing the balance sheet above; current assets 340 000, current liabilities 240 000, stock 120 000, cash 40 000, total balance sheet 840 000):
- General liquidity = 340 / 240 = 1,42.
- Reduced liquidity = (340 - 120) / 240 = 0,92.
- Immediate liquidity = 40 / 240 = 0,17.
- Autonomy = 400 / 840 x 100 = 47,6 %.
- Debt ratio = (200 + 170 + 70) / 400 = 1,10. The 70 is the overdraft.

## 4. Profitability and turnover ratios

- ROE = net result / equity x 100.
- ROA = operating result / total assets x 100.
- Commercial margin rate = (sales of goods - cost of goods sold) / sales of goods x 100.
- Net margin = net result / turnover x 100.
- Days sales outstanding (DSO) = client receivables / turnover including VAT x 360.
- Days payables outstanding (DPO) = supplier payables / purchases including VAT x 360.
- Stock turnover (days) = average stock / cost of goods sold x 360.

Worked example: turnover HT 1 000 000 (TTC 1 200 000 at 20 %); client receivables 150 000. DSO = 150 000 / 1 200 000 x 360 = 45 days. Purchases HT 600 000 (TTC 720 000); supplier payables 110 000. DPO = 110 000 / 720 000 x 360 = 55 days. A DPO longer than the DSO means suppliers finance part of the cycle.

Use 360 days for these ratios, as several accounting references do, and say so.

## 5. Intermediate management balances (SIG)

Cascade from turnover to net result:

1. Commercial margin = sales of goods - cost of goods sold.
2. Production of the period = sold production + stored production + capitalised production.
3. Consumption of the period = purchases of raw materials and supplies + external services (adjusted for stock variations).
4. Value added (valeur ajoutée) = commercial margin + production - consumption.
5. EBE (excédent brut d'exploitation) = value added + operating subsidies - taxes and duties - personnel costs.
6. Operating result = EBE + other operating income - other operating expenses - allowances for depreciation and provisions + reversals.
7. Financial result = financial income - financial charges.
8. Current result before tax = operating result + financial result.
9. Net result = current result + non-current result - tax on profit.

Worked example (synthetic, DH): sales of goods 800 000; cost of goods sold 480 000; commercial margin 320 000. Production sold 200 000. Consumption 250 000. Value added = 320 000 + 200 000 - 250 000 = 270 000. Taxes and duties 20 000; personnel 110 000. EBE = 270 000 - 20 000 - 110 000 = 140 000. Depreciation 40 000. Operating result = 100 000. Financial result -20 000. Current result 80 000. Tax and other items 20 000. Net result 60 000.

## 6. Self-financing capacity (CAF)

CAF = net result + allowances for depreciation and provisions - reversals of provisions - gains on disposal of fixed assets + losses on disposal of fixed assets. Non-cash items are added back.

Worked example: net result 60 000 + depreciation 40 000 = CAF 100 000 (no reversals or disposals in the example).

The CAF is the cash the business generates before paying for working capital. Compare it with investment needs and debt repayments.

## 7. Break-even (seuil de rentabilité)

- Contribution margin = turnover - variable costs.
- Contribution margin rate (TMCV) = contribution margin / turnover.
- Break-even turnover (SR) = fixed costs / TMCV.
- Safety margin = (actual turnover - SR) / actual turnover x 100.
- Break-even in days = SR / turnover x 360.

Worked example (synthetic): turnover 1 000 000; variable costs 600 000. Contribution margin 400 000; TMCV 40 %. Fixed costs 320 000. SR = 320 000 / 0,40 = 800 000. Safety margin = (1 000 000 - 800 000) / 1 000 000 = 20 %. Break-even in days = 800 000 / 1 000 000 x 360 = 288 days. Result at this turnover = (1 000 000 - 800 000) x 40 % = 80 000.

Required turnover for a target profit = (fixed costs + target profit) / TMCV. For a target of 80 000 with the figures above: (320 000 + 80 000) / 0,40 = 1 000 000.

## 8. Method and checks

- Start from the source statements. Write down the period, currency, and whether the figures are audited, management, or draft.
- Check the balance: assets equal liabilities and equity; SIG result equals the net result in the CPC.
- Explain each ratio in one sentence, with the number and its benchmark.
- Separate the reading (what the numbers show) from the interpretation (why) and the recommendation (what to do). Mark the interpretation as an assumption.
- A negative equity, a negative CAF, or a BFR above turnover needs a clear warning and a question to the owner.

## 9. Stop and ask

- The source is not a balance sheet or CPC, or the years do not match.
- The sector benchmark is needed for a judgement and the owner has not given one.
- The owner wants a credit, investment, or divestment decision: the bot provides analysis and the owner decides. Recommend review with the expert-comptable or the lender.

## 10. Output format

1. Headline: the two or three figures that matter, with their period.
2. Table: indicator, formula, value, comparison, reading.
3. Open points: missing data, assumptions, and questions for the owner.

Sources: Loi de Finances 2026 (BO), CGI, CGNC — verify rates against the current BO.
