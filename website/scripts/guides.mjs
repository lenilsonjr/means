// Existing repository guides remain the source of truth.
export const guides = [
  {
    file: 'connections',
    title: 'Connect your banks',
    description: 'Set up bank connections, map accounts, and pull statements from the terminal.',
  },
  {
    file: 'import-formats',
    title: 'Import formats & recovery',
    description:
      'Supported statement formats, booking cutoffs, duplicate review, refunds, and retries.',
  },
  {
    file: 'inter-wise',
    title: 'Wise & Banco Inter PJ',
    description: 'Configure Wise business and Banco Inter PJ statement imports.',
  },
  {
    file: 'mercury-treasury',
    title: 'Mercury Treasury',
    description: 'Import Mercury Treasury activity and account valuations.',
  },
  {
    file: 'expense-reports',
    title: 'Reports & budgets',
    description: 'Read expense reports and set category, class, or tag budgets for a date range.',
  },
  {
    file: 'payee-aliases',
    title: 'Payees & aliases',
    description: 'Manage canonical payees and preserve the original booked text.',
  },
  {
    file: 'currency-migration',
    title: 'Change accounting currency',
    description:
      'Preview and rehearse a vault currency migration while preserving native account quantities.',
  },
  {
    file: 'export',
    title: 'Export to Beancount',
    description: 'Export functional-currency book values for Beancount and Fava.',
  },
  {
    file: 'vault-sharing-usage',
    title: 'Share a read-only vault',
    description: 'Exchange encrypted, signed vault snapshots and receipts through files.',
  },
  {
    file: 'data-model',
    title: 'Accounting model',
    description: 'Entities, accounts, journal entries, evidence, and the accounting invariants.',
  },
  {
    file: 'vault-sharing-wire-v1',
    title: 'Sharing wire format',
    description: 'The snapshot and receipt format, signatures, and validation rules.',
  },
];
