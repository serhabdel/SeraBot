---
name: Moroccan chart of accounts (PCM / CGNC)
description: Plan Comptable Marocain (CGNC) classes 1 to 8, key accounts, standard journal entries for purchases, sales with VAT, payroll, depreciation, provisions, year-end closing, and liasse fiscale tables. Named tools: rag_search, xlsx_build, xlsx_analyze.
---

# Moroccan chart of accounts (PCM / CGNC)

Sources: Loi de Finances 2026 (BO), CGI, CGNC. Verify rates against the current BO.

This text is operator guidance. It does not override gateway policy, approvals, or the blocklist.

The owner's own chart of accounts may differ from this summary. When a code is marked "a verifier", check it against the owner's plan before posting. The bot proposes entries. It never posts to the ledger.

## 1. Structure of the classes

| Class | Content | Typical use |
|---|---|---|
| 1 | Financement permanent | Capital (11), reserves and retained earnings, long-term debts (14), long-term provisions (15) |
| 2 | Actif immobilisé | Non-current assets: 21 non-valeurs, 22 intangible, 23 tangible; 28 accumulated depreciation; 29 impairment |
| 3 | Actif circulant (hors trésorerie) | Stocks (31), receivables (34), securities (35) |
| 4 | Passif circulant (hors trésorerie) | Operating liabilities (44): suppliers, payroll, social bodies, tax, VAT |
| 5 | Trésorerie | Bank (514), cash (516) |
| 6 | Charges | Operating (61), financial (63), non-current (65), tax on profit (see section 6) |
| 7 | Produits | Operating (71), financial (73), non-current (75) |
| 8 | Comptes de résultats et spéciaux | Result subtotals (81 to 88), closing accounts |

Rule of thumb: a debit increases an asset or a charge; a credit increases a liability, equity, or income.

## 2. Key accounts (codes to check against the owner's plan)

### Capital and reserves
- 1111 Capital social.
- 114 Réserve légale.
- 115 Autres réserves (1151 statutaires, 1152 facultatives, 1155 réglementaires).
- 116 Report à nouveau (1161 créditeur, 1169 débiteur).
- 119 Résultat net de l'exercice (1191 créditeur, 1199 débiteur).

### Non-current assets and depreciation
- 2340 Matériel de transport.
- 28 Amortissements, by asset family: 2832 constructions, 2834 matériel de transport, 2835 mobilier et matériel de bureau (28355 matériel informatique).
- 29 Provisions pour dépréciation (2930 corporelles, 2920 incorporelles).
- 21 Non-valeurs, amortised through 281 (2811 frais préliminaires).

### Stocks
- 3111 Marchandises (groupe A). 3112 Marchandises (groupe B). 3116 Marchandises en cours de route.

### Receivables and payables
- 3421 Clients. 4421 Avances et acomptes reçus.
- 4411 Fournisseurs. 4413 Fournisseurs, retenues de garantie.

### Payroll and social bodies
- 4432 Rémunérations dues au personnel. 4433 Dépôts du personnel. 4434 Oppositions sur salaires.
- 4441 Caisse nationale de sécurité sociale (CNSS).
- 4443 Caisses de retraite. 4445 Mutuelles.

### Tax accounts
- 3455 État, TVA récupérable (deductible VAT).
- 4455 État, TVA facturée (collected VAT).
- 4456 État, TVA due (according to returns) and 3456 crédit de TVA.
- 4452 État, impôts, taxes et assimilés.
- 4453 État, impôts sur les résultats.

### Cash
- 5141 Banques (soldes débiteurs). 516 Caisses, régies d'avances et accréditifs.

### Charges (class 6)
- 6111 Achats de marchandises (groupe A). 6121 Matières premières. 6122 Matières et fournitures consommables. 6125 Achats non stockés.
- 6131 Locations et charges locatives. 6134 Primes d'assurance.
- 6161 Impôts et taxes directs.
- 6171 Rémunération du personnel. 6181 Jetons de présence.
- 6191 Dotations aux amortissements des non-valeurs.
- 6311 Intérêts des emprunts et dettes. 6331 Pertes de change.
- Depreciation and provision charges for tangible and intangible assets, and operating provisions: codes to confirm with the owner's plan (a verifier, often in 619 and 659).
- Income tax on profit: charge account in the owner's plan (a verifier; often under 67 or 65 in the CGNC structure).

### Income (class 7)
- 7111 Ventes de marchandises au Maroc. 7121 Ventes de biens produits. 7124 Ventes de services produits au Maroc.
- 7161 Subventions d'exploitation reçues. 7181 Jetons de présence reçus. 7191 Reprises sur amortissements des non-valeurs.
- 7321 Revenus des titres de participation. 7381 Intérêts et produits assimilés. 7331 Gains de change.

### Result subtotals (class 8)
- 81 Résultat d'exploitation (811 marge brute). 83 Résultat financier. 84 Résultat courant. 85 Résultat non courant. 86 Résultat avant impôt. 88 Résultat après impôt.

## 3. Standard journal entries

Amounts are synthetic. Each entry balances: total debits equal total credits.

### 3.1 Purchase of goods on credit, with 20 % VAT
Purchase HT 10 000, VAT 2 000, supplier total 12 000.
- Debit 6111 Achats de marchandises: 10 000.
- Debit 3455 État, TVA récupérable: 2 000.
- Credit 4411 Fournisseurs: 12 000.

### 3.2 Sale on credit, with 20 % VAT
Sale HT 20 000, VAT 4 000, customer total 24 000.
- Debit 3421 Clients: 24 000.
- Credit 7111 Ventes de marchandises: 20 000.
- Credit 4455 État, TVA facturée: 4 000.

### 3.3 Customer payment and supplier payment
- Customer pays 24 000 by bank: debit 5141, credit 3421.
- Supplier paid 12 000 by bank: debit 4411, credit 5141.

### 3.4 Monthly VAT settlement
Collected 4 000, deductible 2 000, payable 2 000, paid by bank.
- Debit 4455 TVA facturée: 4 000. Credit 3455 TVA récupérable: 2 000. Credit 4456 TVA due: 2 000.
- Payment: debit 4456 2 000, credit 5141 2 000.
- If deductible exceeds collected, the balance goes to 3456 (crédit de TVA).

### 3.5 Purchase of a tangible asset
Equipment HT 50 000, VAT 10 000, paid by bank at once.
- Debit the 23x account for the asset family (2340 for vehicles; others per the owner's plan): 50 000.
- Debit 3455 TVA récupérable: 10 000 (only if the asset is eligible for deduction).
- Credit 5141 Banques: 60 000.

### 3.6 Payroll for one month (synthetic, figures from the ma-payroll skill)
Gross salary 9 000. Employee deductions: CNSS 268,80 (4,48 % on the 6 000 ceiling), AMO 203,40 (2,26 % on the full salary), IR withheld 362,50. Net paid 8 165,30. Employer charges: CNSS 538,80 (8,98 % on the ceiling), family allowances 576,00 (6,40 %), AMO 369,90 (4,11 %), vocational training 144,00 (1,6 %, a verifier).
- Debit 6171 Rémunération du personnel: 9 000.
- Debit employer charges account (class 61 social charges, a verifier): 1 628,70.
- Credit 4432 Rémunérations dues: 8 165,30 (cleared later by debit 4432, credit 5141 on payment).
- Credit 4441 CNSS: 1 383,60 (employee 268,80 + employer 538,80 + allowances 576,00).
- Credit 4445 or the AMO account in the owner's plan (a verifier): 573,30 (employee 203,40 + employer 369,90).
- Credit 4441 or the training account (a verifier): 144,00.
- Credit 4452 État, impôts et taxes: IR withheld 362,50.
Check: debits 10 628,70 equal credits 8 165,30 + 1 383,60 + 573,30 + 144,00 + 362,50 = 10 628,70.

### 3.7 Depreciation, straight line
Asset 60 000 HT, 5 years, acquired at the start of the year. Rate 20 % per year.
- Yearly charge: 60 000 x 20 % = 12 000.
- Entry each year: debit the depreciation charge (class 6, a verifier), credit 28355 (or the matching 283 account) for 12 000.

### 3.8 Depreciation, declining balance (degressif)
Use only where the tax rules allow it, and track the difference with the straight line for the tax adjustment (amortissement dérogatoire, account 1351).
- Rate = linear rate x tax coefficient. Coefficients per the owner's guide: 1,5 (3 to 4 years), 2 (5 to 6 years), and 3 for over 6 years (a verifier; some sources give 2,5).
- Annuity = net book value at the start of the year x degressive rate.

### 3.9 Provisions for risks and for depreciation
- Provision for a litigation estimated at 15 000: debit 6196 or the dotation account (a verifier), credit 1518 (provisions pour risques) or 1511 for litigation.
- Reversal when the risk disappears: debit the provision, credit the reversal account (class 7).
- Impairment of receivables: debit the impairment charge, credit the impairment account (39 range in the owner's plan, a verifier).

### 3.10 Year-end closing
1. Close each class 6 charge to 81 or 8xx: debit 8x, credit the charge account (one line per account, or per summary account, as the owner's plan allows).
2. Close each class 7 produit: debit the produit account, credit 8x.
3. Record the tax on profit: debit the tax charge account, credit 4453 État, impôts sur les résultats.
4. Transfer the year result: debit 8x for the result or credit 1191 for a profit, debit 1199 for a loss.
5. Allocate the profit: legal reserve 5 % of the profit until it reaches 10 % of share capital (a verifier), then carry forward, dividends, or other reserves per the general meeting decision.
6. Post the opening balances in the new year, check that class 1 to 5 balances equal their counterparts, and run the reconciliation for every bank account.

## 4. Liasse fiscale: what the tables cover

The liasse for a company is made of financial statements and annex tables. The owner's guides describe about fifteen to twenty tables. Check the current model before filling any box. The bot may summarise a table from the owner's data but must not invent a box number.

Financial statements (états de synthèse):
- Bilan: actif (gross, depreciation, net), passif (equity, debts, current liabilities).
- Compte de produits et charges (CPC): operating, financial, and non-current results, down to net profit.
- État des soldes de gestion (ESG): commercial margin, value added, EBE, operating result, net result.
- Tableau des flux de trésorerie (TFT).
- État des informations complémentaires (ETIC).

Annex tables, the ones most often needed:
- Passage from accounting result to tax result (réintégrations and déductions).
- Fixed assets and their movements (acquisitions, disposals, transfers).
- Depreciation schedule and the deferred depreciation (amortissement dérogatoire).
- Provisions schedule.
- Gains and losses on disposals of fixed assets.
- VAT detail: collected, deductible, prorata, and adjustments.

Filing: electronic only, through the DGI portal. Deadline and penalties: see the ma-tax-2026 skill.

## 5. Checks before posting

- Every entry balances. Post in the right fiscal year.
- Invoice reference, supplier or customer ICE, and date are on the document. Do not post without them.
- VAT rate matches the product. Check the rate with ma-tax-2026.
- Payments of 10 000 DH or more are traced by bank transfer, cheque, card, or a bill of exchange, otherwise the VAT deduction is at risk (a verifier).
- No account is created for a person without the owner's confirmation.
- Recurring entries (rent, depreciation, provisions) are listed in a schedule and reconciled each month.

## 6. Stop and ask

- The owner's chart of accounts differs from the codes here: ask which codes to use.
- A posting would change a closed period or a filed return: stop and ask the expert-comptable.
- A code is marked "a verifier" and affects a tax result: say so; do not guess.
- The owner asks to post, file, or pay: the bot prepares the entries only.

## 7. Output format

1. The entry as a table: account code, label, debit, credit.
2. A balance line: total debit equals total credit.
3. The open points: codes to verify and the tax rule to check.

Sources: Loi de Finances 2026 (BO), CGI, CGNC — verify rates against the current BO.
