from decimal import Decimal

from invoices.reports import Invoice, monthly_tax, render
from invoices.tax import invoice_tax

MONTH = [
    Invoice("A-101", "Initech", (Decimal("120.00"), Decimal("80.50"))),
    Invoice("A-102", "Globex", (Decimal("2500.03"),) * 4),  # 10,000.12: a FIN-12 invoice
    Invoice("A-103", "Initech", (Decimal("10.03"),) * 3),
    Invoice("A-104", "Hooli", (Decimal("999.99"),)),
]


def test_report_reconciles_with_invoices():
    # FIN-3: per customer, the report equals what the invoices charged.
    expected: dict[str, Decimal] = {}
    for inv in MONTH:
        expected[inv.customer] = expected.get(inv.customer, Decimal("0.00")) + invoice_tax(list(inv.lines))
    assert monthly_tax(MONTH) == expected


def test_report_groups_by_customer():
    assert set(monthly_tax(MONTH)) == {"Initech", "Globex", "Hooli"}


def test_empty_month():
    assert monthly_tax([]) == {}
    assert render([]) == ""


def test_render_is_alphabetical():
    assert [line.split(":")[0] for line in render(MONTH).splitlines()] == ["Globex", "Hooli", "Initech"]


def test_render_formats_money():
    assert "Hooli: 180.00" in render(MONTH)
