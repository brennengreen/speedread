--!strict
local Players = game:GetService("Players")
local Signal = require(script.Parent.Signal)

-- Account state.
export type State = {
	balance: number,
	owner: string?,
}

type Callback<T> = (T) -> ()

local Account = {}
Account.__index = Account

-- Creates an account.
function Account.new(owner: string): Account
	local self = setmetatable({}, Account)
	self.owner = owner
	self.balance = 0
	return self
end

@native
function Account:deposit(amount: number): ()
	self.balance += amount
	print(`deposit {amount} @ {self.owner}`)
	return nil
end

Account.reset = function(self: Account)
	self.balance = 0
	self.changed = Signal.new()
	return self
end

local function helper<T>(x: T): T
	local y = x
	local z = y
	return z
end

local scale = @native function(n: number): number
	local m = n * 2
	local k = m + 1
	return k
end

local handlers = {
	onJoin = function(player: Player)
		local name = player.Name
		print(name)
		return name
	end,
}

Players.PlayerAdded:Connect(function(player)
	local acct = Account.new(player.Name)
	acct:deposit(10)
end)

return Account
