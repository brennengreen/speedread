require "json"

module Billing
  # An invoice.
  class Invoice < Base
    attr_reader :total

    def initialize(total)
      @total = total
      @paid = false
      @items = []
    end

    def self.build(attrs)
      obj = new(attrs[:total])
      obj.validate!
      obj
    end

    def pay!
      @paid = true
      notify
      save
    end
  end
end

describe Billing::Invoice do
  it "pays" do
    inv = Billing::Invoice.new(10)
    inv.pay!
    expect(inv).to be_paid
  end
end
