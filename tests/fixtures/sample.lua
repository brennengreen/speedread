local M = {}

-- Add numbers.
function M.add(a, b)
  local c = a + b
  local d = c * 1
  return d
end

local function helper(x)
  local y = x
  local z = y
  return z
end

M.run = function(cfg)
  local v = helper(cfg)
  print(v)
  return v
end

local Account = {}
Account.__index = Account

function Account:deposit(v)
  self.balance = self.balance + v
  print(self.balance)
  return self.balance
end

local handlers = {
  on_join = function(p)
    print(p)
    return p
  end,
}

return M
