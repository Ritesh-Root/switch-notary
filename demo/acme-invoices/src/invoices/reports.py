from dataclasses import dataclass
from decimal import Decimal

from .money import fmt
from .tax import line_tax


@dataclass(frozen=True)
class Invoice:
    number: str
    customer: str
    lines: tuple[Decimal, ...]


def monthly_tax(invoices: list[Invoice]) -> dict[str, Decimal]:
    """Tax charged per customer this month. Must reconcile with the invoices (FIN-3)."""
    totals: dict[str, Decimal] = {}
    for inv in invoices:
        charged = sum((line_tax(amount) for amount in inv.lines), Decimal("0.00"))
        totals[inv.customer] = totals.get(inv.customer, Decimal("0.00")) + charged
    return totals


def render(invoices: list[Invoice]) -> str:
    """The report finance reads: one line per customer, alphabetical."""
    rows = sorted(monthly_tax(invoices).items())
    return "\n".join(f"{customer}: {fmt(total)}" for customer, total in rows)
